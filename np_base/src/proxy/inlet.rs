use crate::net::session_delegate::SessionDelegate;
use crate::net::{tcp_server, udp_server};
use crate::net::{SendMessageFuncType, WriterMessage};
use crate::proxy::crypto::EncryptionMethod;
use crate::proxy::{crypto, InputSenderType};
use crate::proxy::{OutputFuncType, ProxyMessage};
use anyhow::anyhow;
use async_trait::async_trait;
use base64::prelude::*;
use bytes::BytesMut;
use log::{debug, error, trace};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Sender, UnboundedSender};
use tokio::sync::{mpsc, Notify, RwLock};
use tokio::task::yield_now;

const READ_BUF_MAX_LEN: usize = 1024 * 1024;

pub enum InletProxyType {
    TCP,
    UDP,
    // Not implemented
    SOCKS5,
}

impl InletProxyType {
    pub fn from_u32(value: u32) -> Option<InletProxyType> {
        match value {
            0 => Some(InletProxyType::TCP),
            1 => Some(InletProxyType::UDP),
            2 => Some(InletProxyType::SOCKS5),
            _ => None,
        }
    }
}

struct SessionInfo {
    sender: InputSenderType,
    is_compressed: bool,
    encryption_method: EncryptionMethod,
    encryption_key: Vec<u8>,
    read_buf_len: Arc<RwLock<usize>>,
}

type SessionInfoMap = Arc<RwLock<HashMap<u32, SessionInfo>>>;

pub struct Inlet {
    shutdown_tx: Option<Sender<()>>,
    notify: Arc<Notify>,
    session_info_map: SessionInfoMap,
    description: String,
    on_output_callback: OutputFuncType,
}

impl Inlet {
    pub fn new(on_output_callback: OutputFuncType, description: String) -> Self {
        Self {
            shutdown_tx: None,
            notify: Arc::new(Notify::new()),
            session_info_map: Arc::new(RwLock::new(HashMap::new())),
            description,
            on_output_callback,
        }
    }

    pub async fn start(
        &mut self,
        inlet_proxy_type: InletProxyType,
        listen_addr: String,
        output_addr: String,
        is_compressed: bool,
        encryption_method: String,
    ) -> anyhow::Result<()> {
        // 重复调用启动函数
        if self.shutdown_tx.is_some() {
            return Err(anyhow!("Repeated start"));
        }

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let worker_notify = self.notify.clone();
        let session_info_map = self.session_info_map.clone();
        let on_output_callback = self.on_output_callback.clone();
        let is_tcp = match inlet_proxy_type {
            InletProxyType::TCP => true,
            InletProxyType::UDP => false,
            InletProxyType::SOCKS5 => true,
        };

        let create_session_delegate_func = Box::new(move || -> Box<dyn SessionDelegate> {
            Box::new(InletSession::new(
                is_tcp,
                output_addr.clone(),
                session_info_map.clone(),
                is_compressed,
                encryption_method.clone(),
                on_output_callback.clone(),
            ))
        });

        match inlet_proxy_type {
            InletProxyType::TCP => {
                let listener = tcp_server::bind(&listen_addr).await?;
                self.shutdown_tx = Some(shutdown_tx);
                tokio::spawn(async move {
                    tcp_server::run_server(
                        listener,
                        create_session_delegate_func,
                        |stream: TcpStream| async move { Ok(stream) },
                        async move {
                            let _ = shutdown_rx.recv().await;
                        },
                    )
                    .await;
                    worker_notify.notify_one();
                });
            }
            InletProxyType::UDP => {
                let socket = udp_server::bind(&listen_addr).await?;
                self.shutdown_tx = Some(shutdown_tx);
                tokio::spawn(async move {
                    udp_server::run_server(socket, create_session_delegate_func, async move {
                        let _ = shutdown_rx.recv().await;
                    })
                    .await;
                    worker_notify.notify_one();
                });
            }
            InletProxyType::SOCKS5 => {
                return Err(anyhow!("SOCKS5 (Not implemented)"));
            }
        }

        Ok(())
    }

    pub async fn stop(&mut self) {
        if self.shutdown_tx.is_some() {
            self.shutdown_tx.take();
            // 等待退出完毕
            self.notify.notified().await;
        }
    }

    pub fn running(&self) -> bool {
        self.shutdown_tx.is_some()
    }

    pub async fn input(&self, message: ProxyMessage) {
        if let Err(err) = self.input_internal(message).await {
            error!("inlet input error: {}", err.to_string());
        }
    }

    pub fn description(&self) -> &String {
        &self.description
    }

    async fn input_internal(&self, message: ProxyMessage) -> anyhow::Result<()> {
        match message {
            ProxyMessage::O2iConnect(session_id, success, error_msg) => {
                trace!(
                    "O2iConnect: session_id:{session_id}, success:{success}, error_msg:{error_msg}"
                );
                if !success {
                    error!("connect error: {error_msg}");
                    if let Some(session) = self.session_info_map.read().await.get(&session_id) {
                        session.sender.send(WriterMessage::Close)?;
                    }
                }
            }
            ProxyMessage::O2iDisconnect(session_id) => {
                trace!("O2iDisconnect: session_id:{session_id}");
                if let Some(session) = self.session_info_map.read().await.get(&session_id) {
                    session.sender.send(WriterMessage::Close)?;
                }
            }
            ProxyMessage::O2iSendDataResult(session_id, data_len) => {
                // trace!("O2iSendDataResult: session_id:{session_id}, data_len:{data_len}");
                if let Some(session) = self.session_info_map.read().await.get(&session_id) {
                    let mut read_buf_len = session.read_buf_len.write().await;
                    if *read_buf_len <= data_len {
                        *read_buf_len = 0;
                    } else {
                        *read_buf_len = *read_buf_len - data_len;
                    }
                    trace!("O2iSendDataResult: session_id:{session_id}, data_len:{data_len}, read_buf_len:{}", *read_buf_len);
                    drop(read_buf_len);
                }
            }
            ProxyMessage::O2iRecvData(session_id, mut data) => {
                // trace!("O2iRecvData: session_id:{session_id}");
                let data_len = data.len();

                if let Some(session) = self.session_info_map.read().await.get(&session_id) {
                    match session.encryption_method {
                        EncryptionMethod::None => {}
                        _ => {
                            data = crypto::decrypt(
                                &session.encryption_method,
                                session.encryption_key.as_slice(),
                                data.as_slice(),
                            )?;
                        }
                    }
                    if session.is_compressed {
                        data = crypto::decompress_data(data.as_slice())?;
                    }

                    let on_output_callback = self.on_output_callback.clone();
                    let callback: SendMessageFuncType = Box::new(move || {
                        let on_output_callback = on_output_callback.clone();
                        Box::pin(async move {
                            on_output_callback(ProxyMessage::I2oRecvDataResult(
                                session_id, data_len,
                            ))
                            .await;
                        })
                    });

                    session
                        .sender
                        .send(WriterMessage::SendAndThen(data, callback))?;
                } else {
                    trace!("O2iRecvData: unknown session:{session_id}");
                }
            }
            _ => {
                return Err(anyhow!("Unknown message"));
            }
        }

        Ok(())
    }
}

struct InletSession {
    is_tcp: bool,
    output_addr: String,
    session_info_map: SessionInfoMap,
    session_id: u32,
    on_output_callback: OutputFuncType,
    is_compressed: bool,
    encryption_method: EncryptionMethod,
    encryption_key: Vec<u8>,
    read_buf_len: Arc<RwLock<usize>>,
}

impl InletSession {
    pub fn new(
        is_tcp: bool,
        output_addr: String,
        session_info_map: SessionInfoMap,
        is_compressed: bool,
        encryption_method: String,
        on_output_callback: OutputFuncType,
    ) -> Self {
        let encryption_method = crypto::get_method(encryption_method.as_str());
        let encryption_key = crypto::generate_key(&encryption_method);

        Self {
            is_tcp,
            output_addr,
            session_info_map,
            session_id: 0,
            on_output_callback,
            is_compressed,
            encryption_method,
            encryption_key,
            read_buf_len: Arc::new(RwLock::new(0)),
        }
    }
}

#[async_trait]
impl SessionDelegate for InletSession {
    async fn on_session_start(
        &mut self,
        session_id: u32,
        addr: &SocketAddr,
        tx: UnboundedSender<WriterMessage>,
    ) -> anyhow::Result<()> {
        debug!("inlet on session({session_id}) start {addr}");

        self.session_id = session_id;
        self.session_info_map.write().await.insert(
            session_id,
            SessionInfo {
                sender: tx,
                is_compressed: self.is_compressed,
                encryption_method: self.encryption_method.clone(),
                encryption_key: self.encryption_key.clone(),
                read_buf_len: self.read_buf_len.clone(),
            },
        );
        (self.on_output_callback)(ProxyMessage::I2oConnect(
            session_id,
            self.is_tcp,
            self.is_compressed,
            self.output_addr.clone(),
            self.encryption_method.to_string(),
            BASE64_STANDARD.encode(&self.encryption_key),
            addr.to_string(),
        ))
        .await;
        Ok(())
    }

    async fn on_session_close(&mut self) -> anyhow::Result<()> {
        debug!("inlet on session({}) close", self.session_id);
        self.session_info_map.write().await.remove(&self.session_id);
        (self.on_output_callback)(ProxyMessage::I2oDisconnect(self.session_id)).await;
        Ok(())
    }

    fn on_try_extract_frame(&self, buffer: &mut BytesMut) -> anyhow::Result<Option<Vec<u8>>> {
        // 此处使用 buffer.split().to_vec(); 而不是 buffer.to_vec();
        // 因为split().to_vec()更高效，少了一次内存分配和拷贝
        // 并且在 on_try_extract_frame 函数中只能使用消耗 buffer 数据的函数，否则框架会一直循环调用 on_try_extract_frame 来驱动处理消息
        let frame = buffer.split().to_vec();
        Ok(Some(frame))
    }

    async fn on_recv_frame(&mut self, mut frame: Vec<u8>) -> anyhow::Result<()> {
        if self.is_compressed {
            frame = crypto::compress_data(frame.as_slice())?;
        }
        match &self.encryption_method {
            EncryptionMethod::None => {}
            _ => {
                frame = crypto::encrypt(
                    &self.encryption_method,
                    self.encryption_key.as_slice(),
                    frame.as_slice(),
                )?;
            }
        }

        while *self.read_buf_len.read().await > READ_BUF_MAX_LEN {
            yield_now().await;
        }

        let mut read_buf_len = self.read_buf_len.write().await;
        *read_buf_len = *read_buf_len + frame.len();
        drop(read_buf_len);

        (self.on_output_callback)(ProxyMessage::I2oSendData(self.session_id, frame)).await;
        Ok(())
    }
}

use crate::session::WriterMessage;
use np_base::generic;
use np_base::message_map::MessageType;
use std::io;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::RwLock;

pub type PlayerId = u32;

#[warn(dead_code)]
pub struct Player {
    tx: Option<UnboundedSender<WriterMessage>>,
    player_id: PlayerId,
    session_id: u32,
}

impl Player {
    pub fn new(player_id: PlayerId) -> Arc<RwLock<Player>> {
        Arc::new(RwLock::new(Player {
            tx: None,
            player_id,
            session_id: 032,
        }))
    }

    // 获取玩家Id
    #[inline]
    pub fn get_player_id(&self) -> PlayerId {
        self.player_id
    }

    // 获取会话id
    #[inline]
    pub fn get_session_id(&self) -> u32 {
        self.session_id
    }

    // 是否在线
    #[inline]
    pub fn is_online(&self) -> bool {
        self.session_id > 0
    }

    // 重置会话信息
    #[inline]
    fn reset_session_info(&mut self) {
        self.session_id = 0;
        self.tx = None;
    }

    // 玩家上线
    pub async fn on_connect_session(
        &mut self,
        session_id: u32,
        tx: UnboundedSender<WriterMessage>,
    ) {
        assert_eq!(self.is_online(), false);
        self.session_id = session_id;
        self.tx = Some(tx);
    }

    // 玩家离线
    pub async fn on_disconnect_session(&mut self) {
        self.reset_session_info();
    }

    // 玩家被顶号，需要对旧的会话发送一些消息
    pub async fn on_terminate_old_session(&mut self) {
        //

        // 重置会话信息
        self.reset_session_info();
    }

    // 玩家收到消息
    pub async fn on_recv_message(&mut self, message: &MessageType) -> io::Result<MessageType> {
        // 客户端请求的消息，服务器未实现
        Ok(MessageType::GenericError(generic::Error {
            number: generic::ErrorCode::InterfaceAbsent.into(),
            message: "interface absent".into(),
        }))
    }
}

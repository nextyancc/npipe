use crate::global::manager::GLOBAL_MANAGER;
use crate::global::GLOBAL_DB_POOL;
use crate::orm_entity::prelude::Tunnel;
use crate::orm_entity::tunnel;
use crate::player::PlayerId;
use crate::utils::str::{
    get_tunnel_address_port, is_valid_tunnel_endpoint_address, is_valid_tunnel_source_address,
};
use anyhow::anyhow;
use np_proto::message_map::MessageType;
use np_proto::{class_def, server_client};
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, EntityTrait};

pub struct TunnelManager {
    pub tunnels: Vec<tunnel::Model>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self {
            tunnels: Vec::new(),
        }
    }

    pub async fn load_all_tunnel(&mut self) -> anyhow::Result<()> {
        self.tunnels = Tunnel::find().all(GLOBAL_DB_POOL.get().unwrap()).await?;
        Ok(())
    }

    /// 增加通道
    pub async fn add_tunnel(&mut self, mut tunnel: tunnel::Model) -> anyhow::Result<()> {
        if !is_valid_tunnel_source_address(&tunnel.source)
            || !is_valid_tunnel_endpoint_address(&tunnel.endpoint)
        {
            return Err(anyhow!("Address format error"));
        }

        // 端口冲突检测
        if self.port_conflict_detection(
            tunnel.sender,
            get_tunnel_address_port(&tunnel.source),
            None,
        ) {
            return Err(anyhow!("Port Conflict"));
        }

        let new_tunnel = tunnel::ActiveModel {
            id: Default::default(),
            source: Set(tunnel.source.to_owned()),
            endpoint: Set(tunnel.endpoint.to_owned()),
            enabled: Set(tunnel.enabled),
            sender: Set(tunnel.sender),
            receiver: Set(tunnel.receiver),
            description: Set(tunnel.description.to_owned()),
            tunnel_type: Set(tunnel.tunnel_type),
            password: Set(tunnel.password.to_owned()),
            username: Set(tunnel.username.to_owned()),
        };

        let new_tunnel = new_tunnel.insert(GLOBAL_DB_POOL.get().unwrap()).await?;
        tunnel.id = new_tunnel.id;

        Self::broadcast_tunnel_info(tunnel.sender, &tunnel, false).await;
        if tunnel.sender != tunnel.receiver {
            Self::broadcast_tunnel_info(tunnel.receiver, &tunnel, false).await;
        }
        self.tunnels.push(tunnel);

        GLOBAL_MANAGER
            .proxy_manager
            .write()
            .await
            .sync_tunnels(&self.tunnels)
            .await;

        Ok(())
    }

    /// 删除通道
    pub async fn delete_tunnel(&mut self, tunnel_id: u32) -> anyhow::Result<()> {
        let rows_affected = Tunnel::delete_by_id(tunnel_id)
            .exec(GLOBAL_DB_POOL.get().unwrap())
            .await?
            .rows_affected;

        anyhow::ensure!(
            rows_affected == 1,
            "delete_tunnel: rows_affected = {}",
            rows_affected
        );

        if let Some(index) = self.tunnels.iter().position(|it| it.id == tunnel_id) {
            let tunnel = self.tunnels.remove(index);
            Self::broadcast_tunnel_info(tunnel.sender, &tunnel, true).await;
            if tunnel.sender != tunnel.receiver {
                Self::broadcast_tunnel_info(tunnel.receiver, &tunnel, true).await;
            }

            GLOBAL_MANAGER
                .proxy_manager
                .write()
                .await
                .sync_tunnels(&self.tunnels)
                .await;
        }
        return Ok(());
    }

    /// 更新通道
    pub async fn update_tunnel(&mut self, tunnel: tunnel::Model) -> anyhow::Result<()> {
        // 地址合法性检测
        if !is_valid_tunnel_source_address(&tunnel.source)
            || !is_valid_tunnel_endpoint_address(&tunnel.endpoint)
        {
            return Err(anyhow!("Address format error"));
        }
        // 端口冲突检测
        if self.port_conflict_detection(
            tunnel.sender,
            get_tunnel_address_port(&tunnel.source),
            Some(tunnel.id),
        ) {
            return Err(anyhow!("Port Conflict"));
        }

        if let Some(index) = self.tunnels.iter().position(|it| it.id == tunnel.id) {
            let db_tunnel = Tunnel::find_by_id(tunnel.id)
                .one(GLOBAL_DB_POOL.get().unwrap())
                .await?;
            anyhow::ensure!(db_tunnel.is_some(), "Can't find tunnel: {}", tunnel.id);

            let mut db_tunnel: tunnel::ActiveModel = db_tunnel.unwrap().into();
            db_tunnel.source = Set(tunnel.source.to_owned());
            db_tunnel.endpoint = Set(tunnel.endpoint.to_owned());
            db_tunnel.enabled = Set(tunnel.enabled);
            db_tunnel.sender = Set(tunnel.sender);
            db_tunnel.receiver = Set(tunnel.receiver);
            db_tunnel.description = Set(tunnel.description.to_owned());
            db_tunnel.tunnel_type = Set(tunnel.tunnel_type);
            db_tunnel.password = Set(tunnel.password.to_owned());
            db_tunnel.username = Set(tunnel.username.to_owned());
            db_tunnel.update(GLOBAL_DB_POOL.get().unwrap()).await?;

            let old_sender = self.tunnels[index].sender;
            let old_receiver = self.tunnels[index].receiver;

            if old_sender != tunnel.sender {
                Self::broadcast_tunnel_info(old_sender, &tunnel, true).await;
            }
            if old_receiver != tunnel.sender {
                Self::broadcast_tunnel_info(old_receiver, &tunnel, true).await;
            }
            Self::broadcast_tunnel_info(tunnel.sender, &tunnel, false).await;
            if tunnel.sender != tunnel.receiver {
                Self::broadcast_tunnel_info(tunnel.receiver, &tunnel, false).await;
            }

            self.tunnels[index] = tunnel;
            GLOBAL_MANAGER
                .proxy_manager
                .write()
                .await
                .sync_tunnels(&self.tunnels)
                .await;
            return Ok(());
        }
        Err(anyhow!(format!("Unable to find tunnel_id: {}", tunnel.id)))
    }

    async fn broadcast_tunnel_info(player_id: PlayerId, tunnel: &tunnel::Model, is_delete: bool) {
        if player_id != 0 {
            if let Some(player) = GLOBAL_MANAGER
                .player_manager
                .read()
                .await
                .get_player(player_id)
            {
                let _ = player
                    .read()
                    .await
                    .send_push(&MessageType::ServerClientModifyTunnelNtf(
                        server_client::ModifyTunnelNtf {
                            is_delete,
                            tunnel: Some(tunnel.into()),
                        },
                    ))
                    .await;
            }
        }
    }

    /// 检测端口是否冲突
    fn port_conflict_detection(
        &self,
        sender: u32,
        port: Option<u16>,
        tunnel_id: Option<u32>,
    ) -> bool {
        self.tunnels
            .iter()
            .position(|x| {
                x.sender == sender
                    && tunnel_id != Some(x.id)
                    && get_tunnel_address_port(&x.source) == port
            })
            .is_some()
    }

    /// 查询通道
    pub async fn query(&self, page_number: usize, page_size: usize) -> Vec<tunnel::Model> {
        let page_number = page_number;
        let page_size = if page_size <= 0 || page_size > 100 {
            10
        } else {
            page_size
        };
        let start = page_number * page_size;
        let mut end = start + page_size;
        if end > self.tunnels.len() {
            end = self.tunnels.len();
        }

        if start <= end && end <= self.tunnels.len() {
            self.tunnels[start..end].iter().map(|x| x.clone()).collect()
        } else {
            vec![]
        }
    }

    pub fn get_tunnel(&self, id: u32) -> Option<&tunnel::Model> {
        if let Some(index) = self.tunnels.iter().position(|x| x.id == id) {
            Some(&self.tunnels[index])
        } else {
            None
        }
    }
}

impl tunnel::Model {
    pub fn outlet_description(&self) -> String {
        format!(
            "id:{}-sender:{}-enabled:{}",
            self.id, self.sender, self.enabled
        )
    }

    pub fn inlet_description(&self) -> String {
        format!(
            "id:{}-source:{}-endpoint:{}-sender:{}-receiver:{}-tunnel_type:{}-username:{}-password:{}-enabled:{}",
            self.id,
            self.source,
            self.endpoint,
            self.sender,
            self.receiver,
            self.tunnel_type,
            self.username,
            self.password,
            self.enabled
        )
    }
}

impl From<&tunnel::Model> for class_def::Tunnel {
    fn from(tunnel: &tunnel::Model) -> Self {
        Self {
            source: Some(class_def::TunnelPoint {
                addr: tunnel.source.clone(),
            }),
            endpoint: Some(class_def::TunnelPoint {
                addr: tunnel.endpoint.clone(),
            }),
            id: tunnel.id,
            enabled: tunnel.enabled == 1,
            sender: tunnel.sender,
            receiver: tunnel.receiver,
            tunnel_type: tunnel.tunnel_type as i32,
            username: tunnel.username.clone(),
            password: tunnel.password.clone(),
        }
    }
}

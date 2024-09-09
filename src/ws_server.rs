use std::{
    collections::HashMap,
    io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use std::collections::VecDeque;
use tokio::time::{self, Duration};
use tokio::sync::{mpsc, oneshot};
use crate::{ConnId, Key, PlayerId};
use rand::{thread_rng, Rng as _, random};
use sqlx::Sqlite;
use sqlx_core::Error;
use sqlx_core::pool::Pool;

#[derive(Debug)]
enum Command {
    Connect {
        conn_tx: mpsc::UnboundedSender<Key>,
        res_tx: oneshot::Sender<ConnId>,
    },

    Disconnect {
        conn: ConnId,
    },

    AddPlayer {
        id: PlayerId,
        key: Key,
        res_tx: oneshot::Sender<()>,
    },

    Verify {
        key:Key,
        conn_id:ConnId,
        res_tx: oneshot::Sender<(Result<String, Error>)>
    }
}

#[derive(Debug)]
pub struct WsServer {
    /// 链接ID和消息发送管道的键值对
    sessions: HashMap<ConnId, mpsc::UnboundedSender<PlayerId>>,

    /// 客户端key和链接id的键值对
    client_list: HashMap<Key,ConnId>,

    /// 维护的链接总数
    visitor_count: Arc<AtomicUsize>,

    /// 接收命令的管道
    cmd_rx: mpsc::UnboundedReceiver<Command>,

    /// sql命令池
    sql_pool: Arc<Pool<Sqlite>>,

    /// 缓存中的消息队列
    pending_messages: HashMap<Key, VecDeque<PlayerId>>,
}

impl WsServer {
    pub fn new(sql_pool:Arc<Pool<Sqlite>>) -> (WsServer, WsServerHandle) {

        let (cmd_tx,cmd_rx) = mpsc::unbounded_channel();
        (
            WsServer{
                sessions: HashMap::new(),
                client_list: HashMap::new(),
                visitor_count: Arc::new(AtomicUsize::new(0)),
                cmd_rx,
                sql_pool,
                pending_messages: HashMap::new()
            },
            WsServerHandle {
                cmd_tx,
            }
        )
    }
    async fn connect(&mut self, tx: mpsc::UnboundedSender<PlayerId>) -> ConnId{
        // 生成id并插入表
        let id = random::<ConnId>();
        self.sessions.insert(id,tx);
        // 计数器+1
        self.visitor_count.fetch_add(1, Ordering::SeqCst);
        id
    }
    async fn disconnect(&mut self, conn_id: ConnId) {
        // 从表中移除链接
        self.sessions.remove(&conn_id);
    }
    async fn verify(&mut self, key: Key, conn_id:ConnId) -> Result<String,Error>{
        let result: Result<Option<(String, )>, Error> = sqlx::query_as("SELECT name FROM server_info WHERE key = ?")
            .bind(&key)
            .fetch_optional(&*self.sql_pool)
            .await;
        match result{
            Ok(Some(row)) => {
                // 将key和connID的键值对插入表
                self.client_list.insert(key,conn_id);
                Ok(row.0)
        }
            Ok(None) => {
                eprintln!("客户端发送了无效的key，断开链接");
                Err(Error::RowNotFound)
            }
            Err(e) => {
                log::error!("查询客户端密钥失败:{}",e);
                Err(e)
            }
        }
    }
    async fn add_player(&mut self, key: Key, player_id: PlayerId) {
        if let Some(conn_id) = self.client_list.get(&key) {
            if let Some(session) = self.sessions.get(conn_id) {
                if session.send(player_id.clone()).is_err() {
                    self.queue_message(key, player_id).await;
                }
            } else {
                self.queue_message(key, player_id).await;
            }
        } else {
            self.queue_message(key, player_id).await;
        }
    }
    async fn process_pending_messages(&mut self) {
        let mut keys_to_remove = Vec::new();

        for (key, queue) in &mut self.pending_messages {
            if let Some(conn_id) = self.client_list.get(key) {
                if let Some(session) = self.sessions.get(conn_id) {
                    while let Some(player_id) = queue.pop_front() {
                        if session.send(player_id.clone()).is_err() {
                            queue.push_front(player_id);
                            break;
                        }
                    }
                }
            }

            if queue.is_empty() {
                keys_to_remove.push(key.clone());
            }
        }

        for key in keys_to_remove {
            self.pending_messages.remove(&key);
        }
    }

    async fn queue_message(&mut self, key: Key, player_id: PlayerId) {
        self.pending_messages.entry(key).or_default().push_back(player_id);
    }

    pub async fn run(mut self) -> io::Result<()> {
        let mut interval = time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        Command::Connect { conn_tx, res_tx } => {
                            let conn_id = self.connect(conn_tx).await;
                            let _ = res_tx.send(conn_id);
                        }

                        Command::Disconnect { conn } => {
                            self.disconnect(conn).await;
                        }

                        Command::AddPlayer { key, id, res_tx } => {
                            self.add_player(key, id).await;
                            let _ = res_tx.send(());
                        }

                        Command::Verify { key, res_tx, conn_id } => {
                            let res = self.verify(key, conn_id).await;
                            let _ = res_tx.send(res);
                        }
                    }
                }
                _ = interval.tick() => {
                    self.process_pending_messages().await;
                }
            }
        }
    }

}
#[derive(Debug, Clone)]
pub struct WsServerHandle {
    cmd_tx: mpsc::UnboundedSender<Command>,
}

impl WsServerHandle {
    /// 处理来自客户端的连接
    pub async fn connect(&self, conn_tx: mpsc::UnboundedSender<Key>) -> Result<ConnId, io::Error> {
        let (res_tx, res_rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Connect { conn_tx, res_tx })
            .unwrap();

        // unwrap: chat server does not drop out response channel
        Ok( res_rx.await.unwrap())
    }

    /// 验证客户端密钥
    pub async fn verify(&self,key: Key, conn_id: ConnId) -> Result<String, Error> {
        let (res_tx, res_rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Verify {key, conn_id, res_tx })
            .unwrap();

        // unwrap: chat server does not drop out response channel
        let res = res_rx.await.unwrap();
        res
    }

    /// 向特定客户端发送消息
    pub async fn send_message(&self, key: Key, player_id: impl Into<PlayerId>) {
        let (res_tx, res_rx) = oneshot::channel();

        // 将指令发送到指定的客户端
        self.cmd_tx
            .send(Command::AddPlayer {
                id: player_id.into(),
                key,
                res_tx,
            })
            .unwrap();

        // unwrap: chat server does not drop our response channel
        res_rx.await.unwrap();
    }

    /// 断开链接并从服务器注销链接
    pub fn disconnect(&self, conn: ConnId) {
        // unwrap: chat server should not have been dropped
        self.cmd_tx.send(Command::Disconnect { conn }).unwrap();
    }
}
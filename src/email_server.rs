use log;
use tokio::time::{self, Duration};
use tokio::sync::{mpsc, oneshot};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use uuid::Uuid;
use std::collections::HashMap;
use std::{error::Error, io};
use tokio::sync::RwLock;
use std::sync::Arc;
use crate::error::NoSuchValueError;

#[derive(Debug)]
enum Command {
    SendToken {
        email: String,
        res_tx: oneshot::Sender<Result<(), Box<dyn Error + Send + Sync>>>,
    },
    ValidateToken {
        token: String,
        res_tx: oneshot::Sender<Result<(), Box<dyn Error + Send + Sync>>>,
    },
}

pub struct EmailServer {
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    tokens: Arc<RwLock<HashMap<String, (String, time::Instant)>>>,
    smtp_transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl EmailServer {
    pub async fn new(smtp_transport: AsyncSmtpTransport<Tokio1Executor>) -> (EmailServer, EmailServerHandle) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        (
            EmailServer {
                cmd_rx,
                tokens: Arc::new(RwLock::new(HashMap::new())),
                smtp_transport,
            },
            EmailServerHandle {
                cmd_tx,
            },
        )
    }

    pub async fn send_token(&self, email: String) -> Result<(), Box<dyn Error + Send + Sync>> {
        let token = Uuid::new_v4().to_string();
        let message = Message::builder()
            .from("noreply@example.com".parse()?)
            .to(email.parse()?)
            .subject("autowhitelist验证邮件")
            .body(format!(
                "尊敬的用户您好，欢迎注册autowhitelist服务，您的验证链接为: {},只需点击即可完成注册。\n\
                如果您没有注册过相关服务，请忽略本邮件，祝您生活愉快。
            ", token))?;

        self.smtp_transport.send(message).await.map_err(|e| Box::new(e) as Box<dyn Error>)?;

        let mut tokens = self.tokens.write().await;
        tokens.insert(token, (email, time::Instant::now()));
        Ok(())
    }

    pub async fn validate_token(&self, token: String) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut tokens = self.tokens.write().await;
        if tokens.remove(&token).is_some() {
            Ok(())
        } else {
            Err(Box::new(NoSuchValueError))
        }
    }

    pub async fn run(self) -> io::Result<()> {
        let mut interval = time::interval(Duration::from_secs(60));
        let tokens = self.tokens.clone();

        loop {
            tokio::select! {
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        Command::SendToken { email, res_tx } => {
                            let result = self.send_token(email).await;
                            let _ = res_tx.send(result);
                        }
                        Command::ValidateToken { token, res_tx } => {
                            let result = self.validate_token(token).await;
                            let _ = res_tx.send(result);
                        }
                    }
                }
                _ = interval.tick() => {
                    let now = time::Instant::now();
                    let mut tokens = tokens.write().await;
                    tokens.retain(|_, (_, instant)| now.duration_since(*instant) < Duration::from_secs(3600));
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmailServerHandle {
    cmd_tx: mpsc::UnboundedSender<Command>,
}

impl EmailServerHandle {
    // 生成一个新的携带token的链接并发送至指定邮箱
    pub async fn send_token(&self, email: String) -> Result<(), Box<dyn Error + Send + Sync>> {
        let (res_tx, res_rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::SendToken { email, res_tx })
            .unwrap();

        res_rx.await.unwrap()
    }

    // 当用户点击url时验证token合法性
    pub async fn validate_token(&self, token: String) -> Result<(), Box<dyn Error + Send + Sync>> {
        let (res_tx, res_rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::ValidateToken { token, res_tx })
            .unwrap();

        res_rx.await.unwrap()
    }
}
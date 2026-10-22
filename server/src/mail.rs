//! Outgoing mail: SMTP when configured (`SMTP_URL`, `MAIL_FROM`), else logged and kept in an
//! outbox (development and tests).

use std::sync::{Arc, Mutex, PoisonError};

use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::error::{AppError, Result};

#[derive(Clone)]
pub struct Mailer(Arc<Inner>);

enum Inner {
    Log(Mutex<Vec<Mail>>),
    Smtp { transport: AsyncSmtpTransport<Tokio1Executor>, from: Mailbox },
}

#[derive(Clone, Debug)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

impl Mailer {
    pub fn log() -> Self {
        Mailer(Arc::new(Inner::Log(Mutex::new(Vec::new()))))
    }

    /// `url` as lettre reads it: `smtps://user:pass@smtp.example.com:465`.
    pub fn smtp(url: &str, from: &str) -> std::result::Result<Self, String> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url).map_err(|e| format!("SMTP_URL: {e}"))?.build();
        let from = from.parse().map_err(|e| format!("MAIL_FROM: {e}"))?;
        Ok(Mailer(Arc::new(Inner::Smtp { transport, from })))
    }

    pub fn from_env() -> std::result::Result<Self, String> {
        match (std::env::var("SMTP_URL").ok().filter(|v| !v.is_empty()), std::env::var("MAIL_FROM").ok().filter(|v| !v.is_empty())) {
            (Some(url), Some(from)) => Self::smtp(&url, &from),
            (None, None) => {
                tracing::warn!("SMTP_URL and MAIL_FROM unset: mail is logged, not sent");
                Ok(Self::log())
            }
            _ => Err("set both SMTP_URL and MAIL_FROM, or neither".into()),
        }
    }

    pub async fn send(&self, to: &str, subject: &str, body: &str) -> Result<()> {
        match &*self.0 {
            Inner::Log(outbox) => {
                tracing::info!("mail to {to} ({subject}), logged, not sent:\n{body}");
                outbox.lock().unwrap_or_else(PoisonError::into_inner).push(Mail { to: to.into(), subject: subject.into(), body: body.into() });
                Ok(())
            }
            Inner::Smtp { transport, from } => {
                let to: Mailbox = to.parse().map_err(|e| AppError::BadRequest(format!("email: {e}")))?;
                let msg = Message::builder()
                    .from(from.clone())
                    .to(to)
                    .subject(subject)
                    .header(ContentType::TEXT_PLAIN)
                    .body(body.to_string())
                    .map_err(|e| AppError::Internal(format!("mail: {e}")))?;
                transport.send(msg).await.map_err(|e| AppError::Internal(format!("mail: {e}")))?;
                Ok(())
            }
        }
    }

    /// What was "sent" in log mode.
    pub fn outbox(&self) -> Vec<Mail> {
        match &*self.0 {
            Inner::Log(v) => v.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            Inner::Smtp { .. } => Vec::new(),
        }
    }
}

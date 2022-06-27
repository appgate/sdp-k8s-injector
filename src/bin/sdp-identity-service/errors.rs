use k8s_openapi::api::apps::v1::Deployment;
use std::fmt::Display;
use tokio::sync::mpsc::error::SendError;

use crate::identity_manager::IdentityManagerProtocol;

#[derive(Debug)]
pub struct IdentityServiceError {
    who: Option<String>,
    error: String,
}

impl IdentityServiceError {
    pub fn new(error: String, who: Option<String>) -> Self {
        IdentityServiceError {
            error: error,
            who: who,
        }
    }

    pub fn from_string(error: String) -> Self {
        IdentityServiceError {
            error: error,
            who: None,
        }
    }

    pub fn from_service(error: String, who: String) -> Self {
        IdentityServiceError {
            error: error,
            who: Some(who),
        }
    }
}

impl Display for IdentityServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.who.is_some() {
            write!(
                f,
                "IdentityService [{}] error: {}",
                self.who.as_ref().unwrap(),
                self.error
            )
        } else {
            write!(f, "IdentityService error: {}", self.error)
        }
    }
}

impl From<SendError<IdentityManagerProtocol<Deployment>>> for IdentityServiceError {
    fn from(error: SendError<IdentityManagerProtocol<Deployment>>) -> Self {
        IdentityServiceError {
            who: None,
            error: error.to_string(),
        }
    }
}

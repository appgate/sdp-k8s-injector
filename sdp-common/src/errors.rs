use std::fmt::Display;

use crate::sdp::errors::SDPClientError;

#[derive(Debug)]
pub struct SDPServiceError {
    who: Option<String>,
    error: String,
}

impl SDPServiceError {
    pub fn new(error: String, who: Option<String>) -> Self {
        SDPServiceError {
            error: error,
            who: who,
        }
    }

    pub fn from_string(error: String) -> Self {
        SDPServiceError {
            error: error,
            who: None,
        }
    }

    pub fn with_service(self, who: String) -> Self {
        SDPServiceError {
            error: self.error,
            who: Some(who),
        }
    }
}

impl Display for SDPServiceError {
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

impl From<&str> for SDPServiceError {
    fn from(error: &str) -> Self {
        SDPServiceError::from_string(error.to_string())
    }
}

impl From<SDPClientError> for SDPServiceError {
    fn from(error: SDPClientError) -> Self {
        SDPServiceError::from_string(error.to_string())
    }
}

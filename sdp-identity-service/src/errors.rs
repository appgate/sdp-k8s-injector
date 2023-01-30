use std::fmt::Display;

use kube::Error;
use sdp_common::kubernetes::Target;
use sdp_common::{crd::ServiceIdentity, errors::SDPServiceError, sdp::errors::SDPClientError};
use tokio::sync::mpsc::error::SendError;

use crate::identity_manager::IdentityManagerProtocol;

const IDENTITY_SERVICE_MANAGER: &str = "IdentityServiceManager";

pub struct IdentityServiceError(SDPServiceError);

impl From<SendError<IdentityManagerProtocol<Target, ServiceIdentity>>> for IdentityServiceError {
    fn from(error: SendError<IdentityManagerProtocol<Target, ServiceIdentity>>) -> Self {
        IdentityServiceError(
            SDPServiceError::from_string(error.to_string())
                .with_service(IDENTITY_SERVICE_MANAGER.to_string()),
        )
    }
}

impl From<&str> for IdentityServiceError {
    fn from(error: &str) -> Self {
        IdentityServiceError(
            SDPServiceError::from_string(error.to_string())
                .with_service(IDENTITY_SERVICE_MANAGER.to_string()),
        )
    }
}

impl From<String> for IdentityServiceError {
    fn from(error: String) -> Self {
        IdentityServiceError(
            SDPServiceError::from_string(error.clone())
                .with_service(IDENTITY_SERVICE_MANAGER.to_string()),
        )
    }
}

impl From<SDPServiceError> for IdentityServiceError {
    fn from(error: SDPServiceError) -> Self {
        IdentityServiceError(error.with_service(IDENTITY_SERVICE_MANAGER.to_string()))
    }
}

impl From<SDPClientError> for IdentityServiceError {
    fn from(error: SDPClientError) -> Self {
        IdentityServiceError::from(error.to_string())
    }
}

impl From<Error> for IdentityServiceError {
    fn from(error: Error) -> Self {
        IdentityServiceError::from(error.to_string())
    }
}

impl Display for IdentityServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

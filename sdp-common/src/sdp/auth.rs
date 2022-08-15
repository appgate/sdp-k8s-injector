use crate::constants::SDP_IDENTITY_MANAGER_SECRETS;
use crate::service::ServiceCredentialsRef;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LoginUser {
    pub name: String,
    pub need_two_factor_auth: bool,
    pub can_access_audit_logs: bool,
}

type Token = String;

/// Token we obtain after login in SDP system
#[derive(Deserialize, Serialize, Debug)]
pub struct Login {
    pub user: LoginUser,
    pub(crate) token: Token,
    pub expires: String,
}

impl Login {
    pub fn has_expired(&self) -> bool {
        false
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub provider_name: String,
    pub device_id: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceUsers {
    pub data: Vec<ServiceUser>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServiceUser {
    pub id: String,
    pub name: String,
    pub labels: HashMap<String, String>,
    #[serde(skip_deserializing)]
    pub password: Option<String>,
    pub disabled: bool,
    #[serde(skip_serializing)]
    pub failed_login_attempts: Option<u32>,
    #[serde(skip_serializing)]
    pub lock_start: Option<String>,
}

impl ServiceUser {
    pub fn new() -> Self {
        let id = Uuid::new_v4();
        let user_name = Uuid::new_v4();
        let password = Uuid::new_v4();
        Self {
            id: id.to_string(),
            labels: HashMap::new(),
            name: user_name.to_string(),
            password: Some(password.to_string()),
            disabled: true,
            failed_login_attempts: None,
            lock_start: None,
        }
    }
}

impl From<&ServiceUser> for ServiceCredentialsRef {
    fn from(service_user: &ServiceUser) -> Self {
        let pw_field = format!("{}-pw", service_user.id);
        let user_field = format!("{}-user", service_user.id);
        Self {
            id: service_user.id.clone(),
            name: service_user.name.clone(),
            secret: SDP_IDENTITY_MANAGER_SECRETS.to_string(),
            user_field: user_field,
            password_field: pw_field,
        }
    }
}

impl From<ServiceCredentialsRef> for ServiceUser {
    fn from(credentials: ServiceCredentialsRef) -> Self {
        ServiceUser {
            id: credentials.id,
            name: credentials.name,
            labels: HashMap::new(),
            password: None,
            disabled: true,
            failed_login_attempts: None,
            lock_start: None,
        }
    }
}

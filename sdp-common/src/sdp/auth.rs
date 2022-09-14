use crate::service::ServiceUser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::system::ClientProfileUrl;

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
pub struct SDPUsers {
    pub data: Vec<SDPUser>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SDPUser {
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

impl SDPUser {
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

impl From<(&SDPUser, &ClientProfileUrl)> for ServiceUser {
    fn from(service_user: (&SDPUser, &ClientProfileUrl)) -> Self {
        Self {
            name: service_user.0.name.clone(),
            password: service_user.0.password.as_ref().unwrap().clone(),
            profile_url: service_user.1.url.clone(),
        }
    }
}

impl From<ServiceUser> for SDPUser {
    fn from(service_user: ServiceUser) -> Self {
        SDPUser {
            id: service_user.name.clone(),
            name: service_user.name,
            labels: HashMap::new(),
            password: None,
            disabled: true,
            failed_login_attempts: None,
            lock_start: None,
        }
    }
}

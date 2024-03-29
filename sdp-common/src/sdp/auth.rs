use crate::service::ServiceUser;
use chrono::{DateTime, Duration, FixedOffset, Utc};
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
    pub expires: DateTime<FixedOffset>,
}

impl Login {
    pub fn is_expired(&self, now: Option<DateTime<Utc>>) -> bool {
        self.expires
            .checked_sub_signed(Duration::minutes(15))
            .map(|dt| now.unwrap_or(Utc::now()) >= dt.with_timezone(&Utc))
            .unwrap_or(false)
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
    pub fn new(id: String, name: Option<String>, disabled: Option<bool>) -> Self {
        let password = Uuid::new_v4();
        Self {
            name: name.unwrap_or(id.clone()),
            id,
            labels: HashMap::new(),
            password: Some(password.to_string()),
            disabled: disabled.unwrap_or(true),
            failed_login_attempts: None,
            lock_start: None,
        }
    }

    // Return the prefix of the name for the users we create
    // Check for the user parts (strings between the _ character).
    // If we have 4 (cluster_ns_service_random) return 3 everything except the random part
    // If we have 3 (cluster_ns_service) return it (this is old naming for compatibility support with old versions)
    // Otherwise return the name
    pub fn prefix_name(self: &Self) -> String {
        let parts = self.name.split("_");
        let parts: Vec<&str> = match parts.clone().collect::<Vec<&str>>().len() {
            4 | 3 => parts.take(3).collect(),
            _ => parts.collect(),
        };
        parts.join("_")
    }
}

impl From<&ServiceUser> for SDPUser {
    fn from(service_user: &ServiceUser) -> Self {
        let name = service_user.name.clone();
        SDPUser {
            id: name.clone(),
            name: name,
            labels: HashMap::new(),
            password: None,
            disabled: true,
            failed_login_attempts: None,
            lock_start: None,
        }
    }
}

#[cfg(test)]
mod test {
    use chrono::{DateTime, TimeZone, Utc};

    use super::{Login, LoginUser, SDPUser};

    impl LoginUser {
        fn new(name: &str) -> LoginUser {
            LoginUser {
                name: name.to_string(),
                need_two_factor_auth: false,
                can_access_audit_logs: false,
            }
        }
    }

    #[test]
    fn test_token_is_expired() {
        let l = Login {
            user: LoginUser::new("u1"),
            token: "TOKEN".to_string(),
            expires: DateTime::parse_from_rfc3339("2023-01-19T14:45:00.0000000000Z").unwrap(),
        };
        assert!(l.is_expired(Some(Utc.with_ymd_and_hms(2023, 1, 19, 14, 40, 00).unwrap())));
        assert!(l.is_expired(Some(Utc.with_ymd_and_hms(2023, 1, 19, 14, 46, 00).unwrap())));
        assert!(l.is_expired(Some(Utc.with_ymd_and_hms(2023, 1, 19, 14, 50, 00).unwrap())));
        assert!(l.is_expired(Some(Utc.with_ymd_and_hms(2023, 1, 19, 14, 30, 00).unwrap())));
        assert!(!l.is_expired(Some(Utc.with_ymd_and_hms(2023, 1, 19, 14, 29, 59).unwrap())));
    }

    #[test]
    fn test_prefix_name() {
        assert_eq!(
            SDPUser::new("bat".to_string(), None, None).prefix_name(),
            "bat"
        );
        assert_eq!(
            SDPUser::new("bat_bi".to_string(), Some("bat".to_string()), None).prefix_name(),
            "bat"
        );
        assert_eq!(
            SDPUser::new("bat_bi".to_string(), None, None).prefix_name(),
            "bat_bi"
        );
        assert_eq!(
            SDPUser::new("bat_bi".to_string(), Some("bat_bi_hiru".to_string()), None).prefix_name(),
            "bat_bi_hiru"
        );
        assert_eq!(
            SDPUser::new("bat_bi_hiru".to_string(), None, None).prefix_name(),
            "bat_bi_hiru"
        );
        assert_eq!(
            SDPUser::new("bat_bi_hiru_lau".to_string(), None, None).prefix_name(),
            "bat_bi_hiru"
        );
        assert_eq!(
            SDPUser::new("id".to_string(), Some("bat_bi_hiru_lau".to_string()), None).prefix_name(),
            "bat_bi_hiru"
        );
        assert_eq!(
            SDPUser::new(
                "id".to_string(),
                Some("bat_bi_hiru_lau".to_string()),
                Some(true)
            )
            .prefix_name(),
            "bat_bi_hiru"
        );
        assert_eq!(
            SDPUser::new(
                "id".to_string(),
                Some("bat_bi_hiru_lau_bost".to_string()),
                None
            )
            .prefix_name(),
            "bat_bi_hiru_lau_bost"
        );
        assert_eq!(
            SDPUser::new(
                "id".to_string(),
                Some("bat_bi_hiru_lau_bost_sei".to_string()),
                None
            )
            .prefix_name(),
            "bat_bi_hiru_lau_bost_sei"
        );
        assert_eq!(
            SDPUser::new(
                "id".to_string(),
                Some("bat_bi_hiru_lau_bost_sei".to_string()),
                Some(true)
            )
            .prefix_name(),
            "bat_bi_hiru_lau_bost_sei"
        );
        assert_eq!(
            SDPUser::new(
                "id".to_string(),
                Some("bat-bi-hiru-lau".to_string()),
                Some(true)
            )
            .prefix_name(),
            "bat-bi-hiru-lau"
        );
    }
}

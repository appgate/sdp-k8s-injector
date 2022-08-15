use std::collections::HashMap;
use std::fmt::Display;
use std::time::Duration;

use http::header::{InvalidHeaderValue, ACCEPT};
use http::{HeaderValue, StatusCode};
use log::info;
use reqwest::header::HeaderMap;
use reqwest::{Client, Error as RError, Response, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub struct SDPClientError {
    request_error: Option<RError>,
    status_code: Option<reqwest::StatusCode>,
    error_body: Option<String>,
}

impl Display for SDPClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.request_error.is_some() {
            write!(
                f,
                "SDPClient error: {:?}",
                self.request_error.as_ref().unwrap()
            )
        } else {
            write!(
                f,
                "SDPClient error [{:?}]: {:?}",
                self.status_code, self.error_body
            )
        }
    }
}

impl From<RError> for SDPClientError {
    fn from(error: RError) -> Self {
        SDPClientError {
            request_error: Some(error),
            status_code: None,
            error_body: None,
        }
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LoginUser {
    name: String,
    need_two_factor_auth: bool,
    can_access_audit_logs: bool,
}

type Token = String;

/// Token we obtain after login in SDP system
#[derive(Deserialize, Serialize, Debug)]
pub struct Login {
    user: LoginUser,
    token: Token,
    expires: String,
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

#[derive(Clone, Debug)]
pub struct SystemConfig {
    pub hosts: Vec<Url>,
    pub api_version: Option<String>,
    pub credentials: Option<Credentials>,
}

impl SystemConfig {
    pub fn new(hosts: Vec<Url>) -> SystemConfig {
        SystemConfig {
            hosts: hosts,
            api_version: None,
            credentials: None,
        }
    }

    fn headers(&self) -> Result<HeaderMap, InvalidHeaderValue> {
        let mut hm = HeaderMap::new();
        let api_version = self
            .api_version
            .as_ref()
            .map(|v| v.as_str())
            .unwrap_or("17");
        let header_value = format!("application/vnd.appgate.peer-v{}+json", api_version);
        hm.append(ACCEPT, HeaderValue::from_str(&header_value)?);
        Ok(hm)
    }

    pub fn with_api_version(&mut self, _: &str) -> &mut SystemConfig {
        let api_version = self
            .api_version
            .as_ref()
            .map(|v| v.as_str())
            .unwrap_or("17");
        self.api_version = Some(api_version.to_string());
        self
    }

    pub fn build(&mut self, credentials: Credentials) -> Result<System, String> {
        let hm = self
            .headers()
            .map_err(|e| format!("Unable to create SDP client: {}", e))?;
        Client::builder()
            .default_headers(hm)
            .danger_accept_invalid_certs(true)
            .build()
            .map_err(|e| format!("Unable to create SDP client: {}", e))
            .map(|c| System {
                hosts: self.hosts.clone(),
                credentials: credentials.clone(),
                client: c,
                login: None,
            })
    }
}

#[derive(Debug)]
pub struct System {
    hosts: Vec<Url>,
    credentials: Credentials,
    client: Client,
    login: Option<Login>,
}

pub enum ResponseData<D> {
    NoContent,
    Entity(D),
}

async fn error_for_status<D: DeserializeOwned>(
    response: Response,
) -> Result<ResponseData<D>, SDPClientError> {
    if response.status().is_client_error() || response.status().is_server_error() {
        let status = response.status();
        let body = response.text().await?;
        Err(SDPClientError {
            request_error: None,
            status_code: Some(status),
            error_body: Some(body),
        })
    } else if response.status() == StatusCode::NO_CONTENT {
        Ok(ResponseData::NoContent)
    } else {
        Ok(ResponseData::Entity(response.json::<D>().await?))
    }
}

impl System {
    /// /login
    /// Remove the clone!
    async fn login(&self) -> Result<Login, SDPClientError> {
        let mut url = Url::from(self.hosts[0].clone());
        url.set_path("/admin/login");
        let credentials = &self.credentials;
        let resp = self
            .client
            .post(url.clone())
            .json(credentials)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(SDPClientError::from)?;
        match error_for_status::<Login>(resp).await? {
            ResponseData::NoContent => Err(SDPClientError {
                request_error: None,
                status_code: Some(StatusCode::NO_CONTENT),
                error_body: Some("Expected Login instance, found nothing!".to_string()),
            }),
            ResponseData::Entity(login) => Ok(login),
        }
    }

    async fn maybe_refresh_login(&mut self) -> Result<&Login, SDPClientError> {
        if self
            .login
            .as_ref()
            .and_then(|l| l.has_expired().then(|| l))
            .is_some()
            || self.login.is_none()
        {
            info!("Getting a new token!");
            let login = self.login().await?;
            self.login = Some(login);
        }
        Ok(self.login.as_ref().unwrap())
    }

    pub async fn get<D: DeserializeOwned>(&mut self, url: Url) -> Result<D, SDPClientError> {
        let client = self.client.get(url);
        let token = &self.maybe_refresh_login().await?.token;
        let resp = client
            .timeout(Duration::from_secs(5))
            .bearer_auth(token)
            .send()
            .await?;
        match error_for_status::<D>(resp).await? {
            ResponseData::NoContent => Err(SDPClientError {
                request_error: None,
                status_code: Some(StatusCode::NO_CONTENT),
                error_body: Some("Expected instance, found nothing!".to_string()),
            }),
            ResponseData::Entity(data) => Ok(data),
        }
    }

    pub async fn post<D: DeserializeOwned + Serialize>(
        &mut self,
        url: Url,
        data: &D,
    ) -> Result<D, SDPClientError> {
        let client = self.client.post(url);
        let token = &self.maybe_refresh_login().await?.token;
        let resp = client
            .timeout(Duration::from_secs(5))
            .bearer_auth(token)
            .json(&data)
            .send()
            .await?;
        match error_for_status::<D>(resp).await? {
            ResponseData::NoContent => Err(SDPClientError {
                request_error: None,
                status_code: Some(StatusCode::NO_CONTENT),
                error_body: Some("Expected instance, found nothing!".to_string()),
            }),
            ResponseData::Entity(data) => Ok(data),
        }
    }

    pub async fn delete(&mut self, url: Url) -> Result<(), SDPClientError> {
        let client = self.client.delete(url);
        let token = &self.maybe_refresh_login().await?.token;
        let resp = client
            .timeout(Duration::from_secs(5))
            .bearer_auth(token)
            .send()
            .await?;
        match error_for_status::<Option<()>>(resp).await? {
            _ => Ok(()),
        }
    }

    pub async fn put<D: DeserializeOwned + Serialize>(
        &mut self,
        url: Url,
        data: &D,
    ) -> Result<D, SDPClientError> {
        let client = self.client.put(url);
        let token = &self.maybe_refresh_login().await?.token;
        let resp = client
            .timeout(Duration::from_secs(5))
            .bearer_auth(token)
            .json(&data)
            .send()
            .await?;
        match error_for_status::<D>(resp).await? {
            ResponseData::NoContent => Err(SDPClientError {
                request_error: None,
                status_code: Some(StatusCode::NO_CONTENT),
                error_body: Some("Expected instance, found nothing!".to_string()),
            }),
            ResponseData::Entity(data) => Ok(data),
        }
    }

    /// GET /service-users
    pub async fn get_users(&mut self) -> Result<Vec<ServiceUser>, SDPClientError> {
        info!("Getting users");
        let mut url = Url::from(self.hosts[0].clone());
        url.set_path("/admin/service-users");
        let service_users = self.get::<ServiceUsers>(url).await?;
        Ok(service_users.data)
    }

    /// GET /service-users/id
    pub async fn get_user(
        &mut self,
        service_user_id: String,
    ) -> Result<ServiceUser, SDPClientError> {
        info!("Getting user");
        let _ = self.maybe_refresh_login().await?;
        let mut url = Url::from(self.hosts[0].clone());
        url.set_path(&format!("/admin/service-users-id/{}", service_user_id));
        self.get(url).await
    }

    /// POST /service-users/id
    pub async fn create_user(
        &mut self,
        service_user: &ServiceUser,
    ) -> Result<ServiceUser, SDPClientError> {
        let mut url = Url::from(self.hosts[0].clone());
        url.set_path(&format!("/admin/service-users"));
        info!(
            "Creating new ServiceUser in SDP system: {}",
            service_user.id
        );
        self.post::<ServiceUser>(url, service_user).await
    }

    /// POST /service-users/id
    pub async fn modify_user(
        &mut self,
        service_user: &ServiceUser,
    ) -> Result<ServiceUser, SDPClientError> {
        let mut url = Url::from(self.hosts[0].clone());
        url.set_path(&format!("/admin/service-users"));
        info!(
            "Creating new ServiceUser in SDP system: {}",
            service_user.id
        );
        self.put::<ServiceUser>(url, service_user).await
    }

    /// DELETE /service-users/id
    pub async fn delete_user(&mut self, service_user_id: String) -> Result<(), SDPClientError> {
        let _ = self.maybe_refresh_login().await?;
        let mut url = Url::from(self.hosts[0].clone());
        url.set_path(&format!("/admin/service-users/{}", service_user_id));
        self.delete(url).await
    }
}

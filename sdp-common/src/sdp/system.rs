use crate::sdp::auth::{Credentials, Login, ServiceUser, ServiceUsers, ClientProfile};
use crate::sdp::errors::{error_for_status, SDPClientError};
use http::header::{InvalidHeaderValue, ACCEPT};
use http::{HeaderValue, StatusCode};
use log::info;
use reqwest::header::HeaderMap;
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::time::Duration;

const SDP_SYSTEM_HOSTS: &str = "SDP_SYSTEM_HOSTS";
const SDP_SYSTEM_API_VERSION: &str = "v17";
const SDP_SYSTEM_USERNAME: &str = "SDP_K8S_USERNAME";
const SDP_SYSTEM_USERNAME_DEFAULT: &str = "admin";
const SDP_SYSTEM_PASSWORD_ENV: &str = "SDP_K8S_PASSWORD";
const SDP_SYSTEM_PASSWORD_DEFAULT: &str = "admin";
const SDP_SYSTEM_PROVIDER_ENV: &str = "SDP_K8S_PROVIDER";
const SDP_SYSTEM_PROVIDER_DEFAULT: &str = "local";

pub fn get_sdp_system() -> System {
    let hosts = std::env::var(SDP_SYSTEM_HOSTS)
        .map(|vs| {
            vs.split(",")
                .map(|v| Url::parse(v).expect("Error parsing host"))
                .collect::<Vec<Url>>()
        })
        .expect("Unable to get SDP system hosts");

    let credentials = Credentials {
        username: std::env::var(SDP_SYSTEM_USERNAME)
            .unwrap_or(SDP_SYSTEM_USERNAME_DEFAULT.to_string()),
        password: std::env::var(SDP_SYSTEM_PASSWORD_ENV)
            .unwrap_or(SDP_SYSTEM_PASSWORD_DEFAULT.to_string()),
        provider_name: std::env::var(SDP_SYSTEM_PROVIDER_ENV)
            .unwrap_or(SDP_SYSTEM_PROVIDER_DEFAULT.to_string()),
        device_id: uuid::Uuid::new_v4().to_string(),
    };

    SystemConfig::new(hosts)
        .with_api_version(SDP_SYSTEM_API_VERSION)
        .build(credentials)
        .expect("Unable to create SDP client")
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
            .unwrap_or(SDP_SYSTEM_API_VERSION);
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

/// struct representing a client profile
pub struct ClientProfile {
    pub id: String,
    pub name: String,
    pub identityProvidertName: String,
    pub tags: Vec<String>,
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

    // GET /client-profiles
    pub async fn get_client_profiles(&mut self, tag: Option<&str>) -> Result<Vec<ClientProfile>, SDPClientError> {
        info!("Getting user");
        let _ = self.maybe_refresh_login().await?;
        let mut url = Url::from(self.hosts[0].clone());
        url.set_path(&format!("/admin/client-profiles", service_user_id));
        let xs: Vec<ClientProfile> = self.get(url).await?;
        if tag {
            Ok(xs.iter().filter(|p| p.tags.contains(tag)).collect())
        } else {
            Ok(xs)
        }
    }

    pub async fn create_client_profile(&mut self, client_profile: &ClientProfile) -> Result<ClientProfile, SDPClientError> {
        let mut url = Url::from(self.hosts[0].clone());
        url.set_path(&format!("/admin/client-profiles"));
        info!(
            "Creating new ClientProfile in SDP system: {} [{}]",
            client_profile.name,
            client_profile.identityProvidertName
        );
        self.post::<ServiceUser>(url, service_user).await
    }
}

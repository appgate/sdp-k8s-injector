use crate::sdp::auth::{Credentials, Login, ServiceUser, ServiceUsers};
use crate::sdp::errors::{error_for_status, SDPClientError};
use http::header::{InvalidHeaderValue, ACCEPT};
use http::{HeaderValue, StatusCode};
use log::info;
use reqwest::header::HeaderMap;
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::time::Duration;

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

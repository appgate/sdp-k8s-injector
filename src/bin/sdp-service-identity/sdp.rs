use reqwest::header::HeaderMap;
use reqwest::{Client, Error as RError, Url};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LoginUser {
    name: String,
    need_two_factor_auth: bool,
    can_access_audit_logs: bool,
}

type Token = String;

#[derive(Deserialize, Serialize, Debug)]
pub struct Login {
    user: LoginUser,
    token: Token,
    expires: String,
}

impl Login {
    pub fn expired(&self) -> bool {
        false
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub username: String,
    password: String,
    provider_name: String,
    device_id: Option<String>,
}

#[derive(Clone)]
struct SystemConfig {
    hosts: Vec<Url>,
    api_version: Option<String>,
    credentials: Credentials,
}

pub struct SystemBuilder {
    config: SystemConfig,
}

pub struct System {
    config: SystemConfig,
    client: Client,
    login: Option<Login>,
}

impl SystemBuilder {
    fn build(&self, credentials: Credentials) -> Result<System, String> {
        let mut hm = HeaderMap::new();
        Client::builder()
            .default_headers(hm)
            .build()
            .map_err(|e| format!("Unable to create the client: {:?}", e))
            .map(|c| System {
                config: self.config.clone(),
                client: c,
                login: None,
            })
    }
}

impl System {
    fn headers() -> HeaderMap {
        let mut hm = HeaderMap::new();
        hm
    }

    /// /login
    /// Remove the clone!
    async fn login(&self, creds: &Credentials) -> Result<Login, RError> {
        let resp = self
            .client
            .post(self.config.hosts[0].clone())
            .json(&creds)
            .send()
            .await?;
        resp.json::<Login>().await
    }

    async fn maybe_refresh_login<'a>(&'a mut self) -> Result<(), RError> {
        if let Some(login) = self.login.as_ref().and_then(|l| l.expired().then(|| l)) {
            let login = self.login(&self.config.credentials).await?;
            self.login = Some(login);
        }
        Ok(())
    }

    /// GET /service-users
    pub async fn get_users(&mut self) -> Result<Vec<Credentials>, RError> {
        self.maybe_refresh_login().await?;
        Ok(vec![])
    }

    /// GET /service-users-id
    pub async fn get_user(&mut self) -> Result<Credentials, RError> {
        self.maybe_refresh_login().await?;
        unimplemented!();
    }

    /// POST /service-users-id
    pub async fn create_user(&mut self) -> Result<Credentials, RError> {
        self.maybe_refresh_login().await?;
        unimplemented!();
    }

    /// DELETE /service-user-id
    pub async fn delete_user(&mut self) -> Result<Credentials, RError> {
        self.maybe_refresh_login().await?;
        unimplemented!();
    }
}

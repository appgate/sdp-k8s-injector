use http::Error;
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
}

#[derive(Serialize, Deserialize, Debug)]
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
}

pub struct SystemBuilder {
    config: SystemConfig,
}

pub struct System {
    config: SystemConfig,
    client: Client,
}

impl SystemBuilder {
    fn build(&self) -> Result<System, String> {
        let mut hm = HeaderMap::new();
        Client::builder()
            .default_headers(hm)
            .build()
            .map_err(|e| format!("Unable to crate the client: {:?}", e))
            .map(|c| System {
                config: self.config.clone(),
                client: c,
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
    async fn login(&self, creds: Credentials) -> Result<Login, RError> {
        let resp = self
            .client
            .post(self.config.hosts[0].clone())
            .json(&creds)
            .send()
            .await?;
        resp.json::<Login>().await
    }

    /// GET /service-users
    pub async fn get_users(&self, login: Login) -> Result<Vec<Credentials>, RError> {
        unimplemented!();
    }

    /// GET /service-users-id
    pub async fn get_user(&self) -> Result<Credentials, RError> {
        unimplemented!();
    }

    /// POST /service-users-id
    pub async fn create_user(&self) -> Result<Credentials, RError> {
        unimplemented!();
    }

    /// DELETE /service-user-id
    pub async fn delete_user(&self) -> Result<Credentials, RError> {
        unimplemented!();
    }
}

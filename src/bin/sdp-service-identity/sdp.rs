pub mod sdp {
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
    struct Credentials {
        username: String,
        password: String,
        provider_name: String,
        device_id: String,
    }

    struct SystemConfig {
        hosts: Vec<Url>,
        api_version: Option<String>,
    }

    struct SystemBuilder {
        config: SystemConfig,
    }

    struct System<'a> {
        config: &'a SystemConfig,
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
                    config: &self.config,
                    client: c,
                })
        }
    }

    impl System<'static> {
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
        async fn get_users(&self, login: Login) -> Result<(), RError> {
            unimplemented!();
        }

        /// GET /service-users-id
        async fn get_user(&self) -> () {
            unimplemented!();
        }

        /// POST /service-users-id
        async fn create_user(&self) -> () {
            unimplemented!();
        }

        /// DELETE /service-user-id
        async fn delete_user(&self) -> () {
            unimplemented!();
        }
    }
}

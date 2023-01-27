#[macro_use]
extern crate rocket;

use rocket::response::status;
use std::process::exit;

#[post("/quitquitquit")]
fn quitquitquit() -> status::Accepted<String> {
    struct Quitter;

    impl Drop for Quitter {
        fn drop(&mut self) {
            exit(0)
        }
    }

    let _quitter = Quitter;
    status::Accepted(Some(
        "Received request to /quitquitquit. Terminating process.\n".to_string(),
    ))
}

#[launch]
fn rocket() -> _ {
    rocket::build().mount("/", routes![quitquitquit])
}

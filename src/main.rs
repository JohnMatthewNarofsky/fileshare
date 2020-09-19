//! Now that Rocket works on Stable, I *have* to give it a shot.
use ::rocket::{get, launch};
use ::rocket_contrib::serve::{crate_relative, StaticFiles};

#[get("/")]
fn hello() -> &'static str {
    "Hello, world!"
}

#[launch]
fn rocket() -> ::rocket::Rocket {
    rocket::ignite()
        .mount("/", StaticFiles::from(crate_relative!("/static")))
        .mount("/api", ::rocket::routes![hello])
}

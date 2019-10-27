//#![feature(async_await)]

use tide::App;
use tide::Context;
use tide::EndpointResult;
use serde_json::Value;
use percent_encoding::percent_decode;

macro_rules! curry {
	( $func:ident ( $($param:expr),* ) ) => { || { $func($($param),*) } };
}

fn parse_body_bytes(bytes: &Vec<u8>) -> serde_json::Result<Value> {
		let decoded = percent_decode(bytes).decode_utf8().unwrap();
		let body : String = decoded.to_string().replace("payload=", "");
		serde_json::from_str(&body)
}

fn take_action(notification: Value) {
	println!("Begin take_action");

	let pull_url = match &notification["issue"]["pull_request"]["url"] {
		Value::String(s) => {
			s.to_string()
		},
		_ => {
			println!("Oops, couldn't find a pull url.");
			return;
		}
	};

	let pull_information_raw = match reqwest::get(&pull_url) {
		Ok(mut response) => {
			match response.text() {
				Ok(body) => {
					body
				},
				Err(e) => {
					println!("Oops, couldn't download pull information: {}", e);
					return;
				}
			}
		},
		Err(e) => {
			println!("Oops, couldn't download pull information: {}", e);
			return;
		}
	};

	let pull_information_structured: Value = 
		match serde_json::from_str(&pull_information_raw) {
			Ok(parsed) => {
				parsed
			},
			Err(e) => {
				println!("Oops, couldn't parse pull information: {}", e);
				return;
			}
	};

	let head_sha = &pull_information_structured["head"]["sha"];
	let clone_url = &pull_information_structured["head"]["repo"]["clone_url"];

	match clone_url {
		Value::String(clone_url) => {
			match head_sha {
				Value::String(head_sha) => {
					println!("clone_url: {}", clone_url);
					println!("head_sha: {}", head_sha);
				},
				_ => {
					println!("Oops, couldn't get the head's sha.");
				}
			};
		},
		_ => {
			println!("Oops, couldn't get the head's clone url.");
		}
	};
	println!("End   take_action.");
}

async fn get_slash(mut request : Context<()>) -> EndpointResult<String> {
	if let Ok(body_bytes) = &request.body_bytes().await {
		match parse_body_bytes(body_bytes) {
			Ok(parsed) => {
				println!("Begin spawn(take_action).");
				std::thread::spawn(curry!(take_action(parsed)));
				println!("End   spawn(take_action).");
			}
			Err(e) => { 
				println!("Oops: {}", e);
			}
		}
	}
	Ok(format!("Success"))
}

fn main() {
	let mut server = App::new();
	server.at("/").post(get_slash);
	server.run("localhost:8000");
}

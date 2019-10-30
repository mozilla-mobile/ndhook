use futures::future::{BoxFuture, Future};
use percent_encoding::percent_decode;
use serde_json::Value;
use std::boxed::Box;
use std::io::Result;
use std::pin::Pin;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Output;
use tide::response::IntoResponse;
use tide::App;
use tide::Context;
use tide::Endpoint;
use tide::Response;

macro_rules! box_async {
    {$($t:tt)*} => {
        Box::pin(async move { $($t)* })
    };
}

/// Close over a list of _parameters_ to _func_ and generate a zero-
/// parameter lambda that calls _func_ with _parameters_.
macro_rules! close_over {
	( $func:ident ( $($param:expr),* ) ) => { || { $func($($param),*) } };
}

macro_rules! mkdir {
	( $dir:tt ) => {
		Command::new("/bin/mkdir").arg("-p").arg(&$dir).status()
	};
}

macro_rules! clone_error {
	( $e:ident ) => { std::io::Error::new($e.kind(), $e.to_string()) };
}

fn parse_body_bytes(bytes: &Vec<u8>) -> serde_json::Result<Value> {
	let decoded = percent_decode(bytes).decode_utf8().unwrap();
	let body: String = decoded.to_string().replace("payload=", "");
	serde_json::from_str(&body)
}

struct GitCommand {
	command: Command,
	output: Option<Result<Output>>,
}

impl GitCommand {
	fn build(git_command: &str, git_key: &str) -> Self {
		let mut command = Command::new("git");
		command.arg(git_command);
		Self {
			command,
			output: None,
		}
	}

	fn add_parameter(&mut self, p: &str) -> &Self {
		self.command.arg(p);
		self
	}

	fn set_workarea(&mut self, wa: &str) -> &Self {
		self.command.current_dir(wa);
		self
	}

	fn exec(&mut self) -> &Option<Result<Output>> {
		let result = self.command.output();
		self.output = Some(result);
		&self.output
	}

	fn status(&mut self) -> Option<Result<ExitStatus>> {
		if let Some(output) = &self.output {
			match output {
				Ok(output) => return Some(Result::Ok(output.status)),
				Err(e) => return Some(Result::Err(clone_error!(e))),
			}
		}
		None
	}
}

fn take_action(notification: Value) {
	println!("Begin take_action");

	let pull_url = match &notification["issue"]["pull_request"]["url"] {
		Value::String(s) => s.to_string(),
		_ => {
			println!("Oops, couldn't find a pull url.");
			return;
		}
	};

	let pull_information_raw = match reqwest::get(&pull_url) {
		Ok(mut response) => match response.text() {
			Ok(body) => body,
			Err(e) => {
				println!("Oops, couldn't download pull information: {}", e);
				return;
			}
		},
		Err(e) => {
			println!("Oops, couldn't download pull information: {}", e);
			return;
		}
	};

	let pull_information_structured: Value = match serde_json::from_str(&pull_information_raw) {
		Ok(parsed) => parsed,
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
					let work_area = format!("/tmp/workdir/{}", "temp_id");
					match mkdir!(work_area) {
						Ok(status) => {
							if !status.success() {
								println!("(Ok) Failed to make a working directory: {}", status);
								return;
							}
						}
						Err(e) => {
							println!("(Err) Failed to make a working directory: {}", e);
							return;
						}
					};
					println!("Succeeded in making the work directory.");

					let mut clone = GitCommand::build("clone", "api_key");
					clone.set_workarea(&work_area);
					clone.add_parameter(clone_url);
					clone.add_parameter("./");
					clone.exec();
					match clone.status() {
						Some(Ok(status)) => {
							if !status.success() {
								println!("Failed to clone: {}", status);
								return;
							}
						}
						Some(Err(e)) => {
							println!("Failed to clone: {}", e);
							return;
						}
						_ => {
							println!("status() called before execution.");
							return;
						}
					};

					let mut checkout = GitCommand::build("checkout", "api_key");
					checkout.set_workarea(&work_area);
					checkout.add_parameter(head_sha);
					checkout.exec();
					match checkout.status() {
						Some(Ok(status)) => {
							if !status.success() {
								println!("Failed to checkout: {}", status);
								return;
							}
						}
						Some(Err(e)) => {
							println!("Failed to checkout: {}", e);
							return;
						}
						_ => {
							println!("status() called before execution.");
							return;
						}
					};
				}
				_ => {
					println!("Oops, couldn't get the head's sha.");
				}
			};
		}
		_ => {
			println!("Oops, couldn't get the head's clone url.");
		}
	};
	println!("End   take_action.");
}

struct EnvClosure {
	pub git_key: String,
	pub nd_key: String,
}

impl EnvClosure {
	fn new(git_key: String, nd_key: String) -> Self {
		Self { git_key, nd_key }
	}
}

impl Endpoint<()> for EnvClosure {
	type Fut = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;
	fn call(&self, mut request: Context<()>) -> Self::Fut {
		box_async! {
			if let Ok(body_bytes) = &request.body_bytes().await {
				match parse_body_bytes(body_bytes) {
					Ok(parsed) => {
						println!("Begin spawn(take_action).");
						std::thread::spawn(close_over!(take_action(parsed)));
						println!("End   spawn(take_action).");
					}
					Err(e) => {
						println!("Oops: {}", e);
					}
				}
			}
						println!("I am done generating a response.");
			"Success".to_string().into_response()
		}
	}
}

fn main() {
	let mut server = App::new();
	server
		.at("/")
		.post(EnvClosure::new("git_key".to_string(), "nd_key".to_string()));
	server.run("localhost:8000");
}

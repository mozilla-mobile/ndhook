extern crate slog;
extern crate slog_async;
extern crate slog_term;
extern crate tempdir;

use tempdir::TempDir;

use percent_encoding::percent_decode;
use serde_json::Value;
use slog::{error, info, o, Drain, Logger};
use std::io::Result;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use tide::App;
use tide::Context;
use tide::EndpointResult;
use std::convert::TryFrom;

struct PullRequestComment {
	url: String,
	clone_url: String,
	head_sha: String,
	comment: String,
}

impl TryFrom<Value> for PullRequestComment {
	type Error = String;
	fn try_from(notification: Value) -> std::result::Result<Self, Self::Error> {
		let pr_url = match &notification["issue"]["pull_request"]["url"] {
			Value::String(s) => s,
			_ => {
				return Err(format!("Oops, couldn't find a PR url."));
			}
		};

		let comments_url = match &notification["issue"]["comments_url"] {
			Value::String(s) => s,
			_ => {
				return Err(format!("Oops, couldn't find a comments url."));
			}
		};


		let comment = match &notification["comment"]["body"] {
			Value::String(s) => s,
			_ => {
				return Err(format!("Oops, couldn't the comment body."));
			}
		};


		let pull_information_raw = match reqwest::get(pr_url) {
			Ok(mut response) => match response.text() {
				Ok(body) => body,
				Err(e) => {
					return Err(
						format!("Oops, couldn't download PR information: {}", e)
					);
				}
			},
			Err(e) => {
				return Err(format!("Oops, couldn't download PR information: {}", e));
			}
		};

		let pull_information_structured: Value = match serde_json::from_str(&pull_information_raw) {
			Ok(parsed) => parsed,
			Err(e) => {
				return Err(format!("Oops, couldn't parse PR information: {}", e));
			}
		};

		let head_sha = &pull_information_structured["head"]["sha"];
		let clone_url = &pull_information_structured["head"]["repo"]["clone_url"];

		match clone_url {
			Value::String(clone_url) => match head_sha {
				Value::String(head_sha) => {
					Ok(Self {
						url: comments_url.to_string(),
						clone_url: clone_url.to_string(),
						head_sha: head_sha.to_string(),
						comment: comment.to_string(),
					})
				}
				_ => Err(format!("Oops, couldn't get the PR head's sha.")),
			},
			_ => Err(format!("Oops, couldn't get the PR head's clone url.")),
		}
	}
}

trait ToExitCode {
	fn to_exit_code(&self) -> i32;
}

impl ToExitCode for Result<std::process::ExitStatus> {
	fn to_exit_code(&self) -> i32 {
		/*
		 * This could be more ergonomic using map_or_else.
		 */
		match self {
			Ok(o) => {
				if let Some(res) = o.code() {
					res
				} else {
					o.signal().unwrap()
				}
			}
			Err(e) => e.raw_os_error().unwrap(),
		}
	}
}

fn parse_body_bytes(bytes: &Vec<u8>) -> serde_json::Result<Value> {
	let decoded = percent_decode(bytes).decode_utf8().unwrap();
	let body: String = decoded.to_string().replace("payload=", "");
	serde_json::from_str(&body)
}

fn take_action(state: ServerState, notification: Value) {
	let logger = state.logger;

	info!(logger, "Begin take_action");

	info!(logger, "Begin extract_url_and_sha.");
	let extract_url_and_sha_result = PullRequestComment::try_from(notification);
	if let Err(e) = extract_url_and_sha_result {
		error!(
			logger,
			"Could not extract the URL/SHA from the notification: {}", e
		);
		return;
	}

	let pull_request = extract_url_and_sha_result.unwrap();
	let clone_url = pull_request.clone_url;
	let head_sha = pull_request.head_sha;
	let pr_url = pull_request.url;
	let comment = pull_request.comment;
	info!(logger, "clone_url: {}", clone_url);
	info!(logger, "head_sha: {}", head_sha);
	info!(logger, "pr_url: {}", pr_url);
	info!(logger, "comment: {}", comment);

	if comment != "profile" {
		info!(logger, "Bad command: {}", comment);
		return;
	}

	// Create a directory to build in.
	let temp_dir = TempDir::new("prefix");
	if let Err(e) = temp_dir {
		error!(logger, "(Err) Failed to make a working directory: {}", e);
		return;
	}
	let temp_dir = temp_dir.unwrap();
	let work_area = temp_dir.path();
	info!(logger, "Succeeded in making the work directory.");

	let clone_result = Command::new("git")
		.arg("clone")
		.current_dir(&work_area)
		.arg(clone_url)
		.arg("./")
		.status();
	if clone_result.to_exit_code() != 0 {
		error!(
			logger,
			"Failed to clone: {}",
			std::io::Error::from_raw_os_error(clone_result.to_exit_code())
		);
	}

	let checkout_result = Command::new("git")
		.arg("checkout")
		.current_dir(&work_area)
		.arg(head_sha)
		.arg("./")
		.status();
	if checkout_result.to_exit_code() != 0 {
		error!(
			logger,
			"Failed to checkout: {}",
			std::io::Error::from_raw_os_error(checkout_result.to_exit_code())
		);
	}

	let build_result = Command::new("./gradlew")
		.arg("app:assembleGeckoNightlyFenixNightly")
		.current_dir(&work_area)
		.status();
	if build_result.to_exit_code() != 0 {
		error!(
			logger,
			"Failed to build: {}",
			std::io::Error::from_raw_os_error(build_result.to_exit_code())
		);
	}

	let post_client = reqwest::Client::new();
	match post_client.post(&pr_url).header(reqwest::header::AUTHORIZATION, format!("token {}", state.git_key).to_string()).body("{ \"body\": \"Plus one\" }").send() {
		Ok(o) => {
			info!(logger, "Posted a comment: {:?}", o);
		}
		Err(e) => {
			error!(logger, "Failed to post a comment: {}", e);
		}
	};

	info!(logger, "End   take_action.");
}

#[derive(Clone)]
struct ServerState {
	pub git_key: String,
	pub nd_key: String,
	pub logger: Logger,
}

impl ServerState {
	fn new(git_key: String, nd_key: String, logger: Logger) -> Self {
		Self {
			git_key,
			nd_key,
			logger,
		}
	}
}

async fn handle_post(mut request: Context<ServerState>) -> EndpointResult<String> {
	info!(request.state().logger, "Start handle_post");
	if let Ok(body_bytes) = &request.body_bytes().await {
		match parse_body_bytes(body_bytes) {
			Ok(parsed) => {
				let state = (*request.state()).clone();
				info!(request.state().logger, "Begin spawn(take_action).");
				std::thread::spawn(|| {
					take_action(state, parsed);
				});
				info!(request.state().logger, "End spawn(take_action).");
			}
			Err(e) => {
				error!(request.state().logger, "Oops, could not parse the body of the notification: {}", e);
			}
		}
	}
	info!(request.state().logger, "End handle_post");
	Ok("Success".to_string())
}

fn main() {
	let decorator = slog_term::TermDecorator::new().build();
	let drain = slog_term::FullFormat::new(decorator).build().fuse();
	let drain = slog_async::Async::new(drain).build().fuse();
	let log = slog::Logger::root(drain, o!());

	info!(log, "Starting.");

	let mut server = App::with_state(ServerState::new(
		"git_key".to_string(),
		"nd_key".to_string(),
		log,
	));
	server.at("/").post(handle_post);
	match server.run("localhost:8000") {
		_ => (),
	}
}

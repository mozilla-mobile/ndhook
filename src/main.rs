/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

extern crate nimbledroidrs;
extern crate slog;
extern crate slog_async;
extern crate slog_term;
extern crate tempdir;

use tempdir::TempDir;

use nimbledroidrs::Profiler;
use percent_encoding::percent_decode;
use serde_json::Value;
use slog::{error, info, o, Drain, Logger};
use std::convert::TryFrom;
use std::fs::File;
use std::fs::Permissions;
use std::io::Result;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::time::Duration;
use tide::App;
use tide::Context;
use tide::EndpointResult;

struct PullRequestComment {
	url: String,
	clone_url: String,
	head_sha: String,
	comment: String,
	commenter: String,
}

impl TryFrom<Value> for PullRequestComment {
	type Error = String;
	fn try_from(notification: Value) -> std::result::Result<Self, Self::Error> {
		let pr_url = match &notification["issue"]["pull_request"]["url"] {
			Value::String(s) => s,
			_ => {
				return Err("Oops, couldn't find a PR url.".to_string());
			}
		};

		let comments_url = match &notification["issue"]["comments_url"] {
			Value::String(s) => s,
			_ => {
				return Err("Oops, couldn't find a comments url.".to_string());
			}
		};

		let comment = match &notification["comment"]["body"] {
			Value::String(s) => s,
			_ => {
				return Err("Oops, couldn't find the comment body.".to_string());
			}
		};

		let commenter = match &notification["comment"]["user"]["login"] {
			Value::String(s) => s,
			_ => {
				return Err("Oops, couldn't find the commenter.".to_string());
			}
		};

		let pull_information_raw = match reqwest::get(pr_url) {
			Ok(mut response) => match response.text() {
				Ok(body) => body,
				Err(e) => {
					return Err(format!("Oops, couldn't download PR information: {}", e));
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
				Value::String(head_sha) => Ok(Self {
					url: comments_url.to_string(),
					clone_url: clone_url.to_string(),
					head_sha: head_sha.to_string(),
					comment: comment.to_string(),
					commenter: commenter.to_string(),
				}),
				_ => Err("Oops, couldn't get the PR head's sha.".to_string()),
			},
			_ => Err("Oops, couldn't get the PR head's clone url.".to_string()),
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

fn parse_body_bytes(bytes: &[u8]) -> serde_json::Result<Value> {
	let decoded = percent_decode(bytes).decode_utf8().unwrap();
	let body: String = decoded.to_string().replace("payload=", "");
	serde_json::from_str(&body)
}

#[allow(clippy::cognitive_complexity)]
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
	let commenter = pull_request.commenter;
	info!(logger, "clone_url: {}", clone_url);
	info!(logger, "head_sha: {}", head_sha);
	info!(logger, "pr_url: {}", pr_url);
	info!(logger, "comment: {}", comment);
	info!(logger, "commenter: {}", commenter);

	if comment != "profile" {
		info!(logger, "Bad command: {}", comment);
		return;
	}

	if !state.profilers.contains(&commenter.to_lowercase()) {
		info!(logger, "Bad commenter: {} not found in {:?}", commenter, state.profilers);
		return;
	}

	// Create a directory to build in.
	let temp_dir = TempDir::new("prefix");
	if let Err(e) = temp_dir {
		error!(logger, "(Err) Failed to make an artifact directory: {}", e);
		return;
	}
	let temp_dir = temp_dir.unwrap();
	let artifact_area = temp_dir.path();
	let artifact_area_permissions = Permissions::from_mode(0o733);
	if std::fs::set_permissions(&artifact_area, artifact_area_permissions).is_err() {
		error!(
			logger,
			"(Err) Could not set the permissions on the artifact directory."
		);
		return;
	}
	info!(
		logger,
		"Succeeded in making the artifact directory and setting the permissions."
	);

	let build_result = Command::new("docker")
		.arg("run")
		.arg("--rm")
		.arg("-ti")
		.arg("--volume")
		.arg(format!("{}:/build_output/", artifact_area.display()))
		.arg("3683fdbe380c")
		.arg("/buildtools/build_fenix.sh")
		.arg(clone_url)
		.arg(head_sha)
		.arg("assembleGeckoNightlyFenixNightly")
		.arg("app/build/outputs/apk/*")
		.status();
	if build_result.to_exit_code() != 0 {
		error!(
			logger,
			"Failed to build: {}",
			std::io::Error::from_raw_os_error(build_result.to_exit_code())
		);
	}

	let profile = Profiler::new(
		&state.nd_key,
		&format!(
			"{}/fenixNightly/app-geckoNightly-armeabi-v7a-fenixNightly-unsigned.apk",
			&temp_dir.path().to_str().unwrap()
		),
	);
	let profile_url: reqwest::Url;
	match profile.upload() {
		Ok(url) => profile_url = url,
		Err(e) => {
			error!(logger, "Failed to upload the artifact to ND: {}.", e);
			return;
		}
	}

	let mut comment_string = "".to_string();

	info!(logger, "Starting to wait for the profile.");
	if profile
		.wait_for_profile(&profile_url, Duration::from_secs(1200))
		.is_err()
	{
		comment_string =
			"Timeout while waiting for ND to complete profiling the application.".to_string();
		error!(logger, "{}", comment_string);
	} else {
		info!(logger, "Done waiting for the profile.");

		if let Some(profile_result) = profile.get_profile_result(&profile_url) {
			comment_string.push_str(&"Scenario | Status | Time (ms)\\n".to_string());
			comment_string.push_str(&"---------|--------|----------\\n".to_string());
			for p in profile_result.profiles {
				comment_string.push_str(&format!(
					"{} | {} | {}\\n",
					p.get_scenario_name(),
					p.get_status(),
					p.get_time_in_ms()
				));
			}
		} else {
			comment_string = "Failed to get the results of the profile from ND.".to_string();
			error!(logger, "{}", comment_string);
		}
	}

	let comment_post_client = reqwest::Client::new();
	match comment_post_client
		.post(&pr_url)
		.header(
			reqwest::header::AUTHORIZATION,
			format!("token {}", state.git_key),
		)
		.body(format!("{{ \"body\": \"{}\" }}", comment_string))
		.send()
	{
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
	pub profilers: Vec<String>,
	pub logger: Logger,
}

impl ServerState {
	fn new(git_key: String, nd_key: String, profilers: &[String], logger: Logger) -> Self {
		Self {
			git_key,
			nd_key,
			profilers: profilers.to_vec(),
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
				error!(
					request.state().logger,
					"Oops, could not parse the body of the notification: {}", e
				);
			}
		}
	}
	info!(request.state().logger, "End handle_post");
	Ok("Success".to_string())
}

fn profilers_from_file(filename: &str) -> Vec<String> {
	if let Ok(f) = File::open(filename) {
		if let Ok(profilers) = serde_json::from_reader(f) {
			profilers
		} else {
			vec![]
		}
	} else {
		vec![]
	}
}

fn main() {
	let decorator = slog_term::TermDecorator::new().build();
	let drain = slog_term::FullFormat::new(decorator).build().fuse();
	let drain = slog_async::Async::new(drain).build().fuse();
	let log = slog::Logger::root(drain, o!());

	info!(log, "Starting.");

	let profilers = profilers_from_file("./profilers.json");
	let lc_profilers: Vec<String> = profilers.into_iter().map(|s| s.to_lowercase()).collect();

	let mut server = App::with_state(ServerState::new(
		"git_key".to_string(),
		"nd_key".to_string(),
		&lc_profilers,
		log,
	));
	server.at("/").post(handle_post);
	match server.run("localhost:8000") {
		_ => (),
	}
}

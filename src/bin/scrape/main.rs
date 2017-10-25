extern crate backoff;
extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate indicatif;
extern crate itertools;
extern crate num;
extern crate regex;
extern crate select;
extern crate threadpool;
extern crate tokio_core;
extern crate url;

use std::cell::RefCell;
use std::fs::File;
use std::sync::mpsc::channel;

use futures::{Future, Stream};
use hyper::{Client, Method, Request};
use hyper_tls::HttpsConnector;
use threadpool::ThreadPool;
use tokio_core::reactor::Core;

thread_local!(static CORE: RefCell<Core> = RefCell::new(Core::new().unwrap()));

#[derive(Debug)]
pub enum Error {
	Hyper(hyper::Error),
	Io(std::io::Error),
	String(std::string::FromUtf8Error)
}

impl Error {
	fn to_backoff_error(self) -> backoff::Error<Error> {
		match self {
			Error::Hyper(_) | Error::Io(_) =>
				backoff::Error::Transient(self),
			_ =>
				backoff::Error::Permanent(self)
		}
	}
}

impl From<backoff::Error<Error>> for Error {
	fn from(error: backoff::Error<Error>) -> Self {
		match error {
			backoff::Error::Transient(e) | backoff::Error::Permanent(e) => e
		}
	}
}

impl From<hyper::Error> for Error {
	fn from(error: hyper::Error) -> Self {
		Error::Hyper(error)
	}
}

impl From<std::io::Error> for Error {
	fn from(error: std::io::Error) -> Self {
		Error::Io(error)
	}
}

impl From<std::string::FromUtf8Error> for Error {
	fn from(error: std::string::FromUtf8Error) -> Self {
		Error::String(error)
	}
}

fn get_ssid() -> Result<String, Error> {
	CORE.with(|core| {
		let mut core = core.borrow_mut();
	    let client = Client::configure()
	    	.connector(HttpsConnector::new(4, &core.handle()).unwrap())
	    	.build(&core.handle());

		let url = "https://eswil.ijp.pan.pl/index.php?str=otworz-slownik".parse().unwrap();
		let re = regex::Regex::new(r"PHPSESSID=([a-z0-9]+);").unwrap();

		let future = client
			.get(url)
			.map(|res| {
				let header = res.headers().get::<hyper::header::SetCookie>().unwrap();
				assert_eq!(header.len(), 1);
				re.captures(&header[0]).unwrap().get(1).unwrap().as_str().to_string()
			})
			.from_err::<Error>();

		core.run(future)
	})
}

fn get_page(ssid: &str, letter: char, offset: u8, word: Option<&str>) -> Result<select::document::Document, Error> {
	use url::form_urlencoded::Serializer;

	CORE.with(|core| {
		let mut core = core.borrow_mut();
	    let client = Client::configure()
	    	.connector(HttpsConnector::new(4, &core.handle()).unwrap())
	    	.build(&core.handle());

	    let mut data = Serializer::new(String::new());
	    data.extend_pairs(&[
	    	("idHasla", "0"),
	    	("uklad", "poziomy"),
	    	("offset", &offset.to_string()),
	    	("litera", &letter.to_string()),
	    	("Wsposob", "0"),
	    	("Whaslo", word.unwrap_or("")),
	    	("Wkolejnosc", "a fronte"),
	    	("czesc", "str"),
	    	("str", "3"),
	    	("skala", "100"),
	    	("nowyFiltr", ""),
	    	("hSz", "0")
	    ]);

		let url = "https://eswil.ijp.pan.pl/index.php".parse().unwrap();
		let mut request = Request::new(Method::Post, url);

		{
			let headers = request.headers_mut();
			headers.set(hyper::header::ContentType::form_url_encoded());
			headers.set(hyper::header::UserAgent::new("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/64.0.3247.0 Safari/537.36"));

			let mut cookie = hyper::header::Cookie::new();
			cookie.set("PHPSESSID", ssid.to_string());
			headers.set(cookie);
		}

		request.set_body(data.finish());

		let future = client
			.request(request)
			.and_then(|res| res.body().concat2())
			.from_err::<Error>()
			.and_then(|chunk| Ok(select::document::Document::from_read(chunk.as_ref())?))
			.from_err::<Error>();

		core.run(future)
	})
}

const NUMBER_OF_THREADS: usize = 32;
const LETTERS: &'static str = "ABCĆDEFGHIJKLŁMNOÓPQRSŚTUVWXYZŹŻ";

fn main() {
	use backoff::Operation;
	use select::predicate::{Attr, Child, Name, Predicate};
	use std::io::Write;

	let ssid = get_ssid().unwrap();

	let message = RefCell::new(String::from(LETTERS));
	let char_count = LETTERS.chars().count();

	let pb = indicatif::ProgressBar::new(char_count as u64);
	pb.set_message(&message.borrow());
	pb.set_style(indicatif::ProgressStyle::default_bar().template("{msg} {wide_bar} {pos}/{len}"));

	let pool = ThreadPool::new(NUMBER_OF_THREADS);
	let (tx, rx) = channel();

	for letter in LETTERS.chars() {
		let tx = tx.clone();
		let ssid = ssid.clone();
		pool.execute(move || {
			let mut backoff = backoff::ExponentialBackoff::default();
			let mut op = || -> Result<_, backoff::Error<Error>> {
				get_page(&ssid, letter, 0, None)
					.map_err(|e| e.to_backoff_error())
			};
			
			let document: Result<select::document::Document, Error> = op.retry(&mut backoff).map_err(|e| e.into());
			let re = regex::Regex::new(r"(\d+)-(\d+)/(\d+)").unwrap();
			let left = document.unwrap().find(Attr("id", "listaHasel")).next().unwrap().children().nth(0).unwrap().text();
			let captures = re.captures(left.trim()).unwrap();
			let out_of: u32 = captures.get(3).unwrap().as_str().parse().unwrap();

			tx.send((letter, out_of)).expect("should have sent");
		});
	}

	let counts: std::collections::HashMap<char, u32> = rx
		.into_iter()
		.take(char_count)
		.inspect(|&(letter, _)| {
			pb.inc(1);
			let new_message = message.borrow_mut().replace(letter, "-");
			*message.borrow_mut() = new_message;
			pb.set_message(&message.borrow());
		})
		.collect();
	let total_count: u32 = counts.values().sum();

	pb.finish();

	let pb = indicatif::ProgressBar::new(total_count as u64);
	pb.set_style(indicatif::ProgressStyle::default_bar().template("{eta} {wide_bar} {pos}/{len}"));

	let (tx, rx) = channel();
	for (letter, count) in counts {
		for i in num::range_step(0, count, 200) {
			let tx = tx.clone();
			let ssid = ssid.clone();
			let offset = i / 200;
			pool.execute(move || {
				let mut backoff = backoff::ExponentialBackoff::default();
				let mut op = || -> Result<_, backoff::Error<Error>> {
					get_page(&ssid, letter, offset as u8, None)
						.map_err(|e| e.to_backoff_error())
				};
				
				let document = op.retry(&mut backoff).unwrap();
				let words = document.find(Child(Attr("id", "listaHasel"), Name("div"))).into_selection();
				for node in words.iter().take(words.len() - 1) {
					let a = node.find(Name("a")).filter(|c| c.attr("id").is_none()).next().unwrap();
					let word = a.text();
					tx.send(word).expect("should have sent");
				}
			});
		}
	}

	let mut f = File::create("output").unwrap();

	rx
		.into_iter()
		.take(total_count as usize)
		.inspect(|word| {
			pb.inc(1);
			pb.set_message(word);
		})
		.for_each(|word| { writeln!(&mut f, "{}", word).unwrap(); });

	pb.finish();
}

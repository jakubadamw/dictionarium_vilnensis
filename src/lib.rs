#![feature(async_await)]

use hyper::{Client, Request, Uri};

#[derive(Debug)]
pub enum Error {
    Http(http::Error),
    Hyper(hyper::Error),
    Io(std::io::Error),
    Cookie(CookieHeaderError),
    String(std::string::FromUtf8Error),
    MissingElement(&'static str)
}

impl Error {
    pub fn into_backoff_error(self) -> backoff::Error<Error> {
        match self {
            Error::Http(_) | Error::Hyper(_) | Error::Io(_) =>
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

impl From<http::Error> for Error {
    fn from(error: http::Error) -> Self {
        Self::Http(error)
    }
}

impl From<hyper::Error> for Error {
    fn from(error: hyper::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<std::string::FromUtf8Error> for Error {
    fn from(error: std::string::FromUtf8Error) -> Self {
        Self::String(error)
    }
}

#[derive(Debug)]
pub enum CookieHeaderError {
    InvalidCookie(cookie::ParseError),
    InvalidString(hyper::header::ToStrError),
    MissingCookie
}

impl From<cookie::ParseError> for CookieHeaderError {
    fn from(error: cookie::ParseError) -> Self {
        Self::InvalidCookie(error)
    }
}

impl From<hyper::header::ToStrError> for CookieHeaderError {
    fn from(error: hyper::header::ToStrError) -> Self {
        Self::InvalidString(error)
    }
}

pub async fn get_ssid() -> Result<String, Error> {
    let https = hyper_tls::HttpsConnector::new(4).unwrap();
    let client = Client::builder().build::<_, hyper::Body>(https);

    let url = "https://eswil.ijp.pan.pl/index.php?str=otworz-slownik".parse().unwrap();

    let response = client.get(url).await?;
    let (valid, _) = response
        .headers()
        .get_all(hyper::header::SET_COOKIE)
        .iter()
        .map(|value| Ok(cookie::Cookie::parse_encoded(value.to_str()?)?))
        .partition::<Vec<Result<_, CookieHeaderError>>, _>(Result::is_ok);

    valid
        .into_iter()
        .map(Result::unwrap) // Safe per definition.
        .filter(|cookie| cookie.name() == "PHPSESSID")
        .map(|cookie| cookie.value().to_string())
        .next()
        .ok_or(Error::Cookie(CookieHeaderError::MissingCookie))
}

pub async fn get_page<S: AsRef<str>>(ssid: S, letter: char, offset: u16, word: Option<u32>) -> Result<select::document::Document, Error> {
    use futures::future::TryFutureExt;
    use futures::stream::TryStreamExt;
    use url::form_urlencoded::Serializer;

    const USER_AGENT: &str =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
         AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/64.0.3247.0 Safari/537.36";
    const URL: &str = "https://eswil.ijp.pan.pl/index.php";

    let https = hyper_tls::HttpsConnector::new(4).unwrap();
    let client = Client::builder().build::<_, hyper::Body>(https);

    let mut data = Serializer::new(String::new());
    data.extend_pairs(&[
        ("idHasla", word.unwrap_or(0).to_string().as_str()),
        ("uklad", "poziomy"),
        ("offset", offset.to_string().as_str()),
        ("litera", letter.to_string().as_str()),
        ("Wsposob", "0"),
        ("Whaslo", ""),
        ("Wkolejnosc", "a fronte"),
        ("czesc", "str"),
        ("str", "3"),
        ("skala", "100"),
        ("nowyFiltr", ""),
        ("hSz", "0")
    ]);

    let uri: Uri = URL.parse().expect("the hard-coded URL must be valid!");

    let request = Request::post(uri)
        .header(hyper::header::CONTENT_TYPE,
            mime::APPLICATION_WWW_FORM_URLENCODED.as_ref())
        .header(hyper::header::USER_AGENT,
            USER_AGENT)
        .header(hyper::header::COOKIE,
            cookie::Cookie::new("PHPSESSID", ssid.as_ref().to_owned()).to_string())
        .body(data.finish().into())?;

    let body = client
        .request(request)
        .and_then(|res| res.into_body().try_concat())
        .await?;

    Ok(select::document::Document::from_read(body.as_ref())?)
}

pub async fn scrape_letter_count_with_backoff(ssid: String, letter: char) -> (char, u64) {
    use backoff_futures::TryFutureExt as _;
    use futures::future::TryFutureExt;
    use select::predicate::Attr;

    let mut backoff = backoff::ExponentialBackoff::default();
    let document = (|| get_page(&ssid, letter, 0, None).map_err(Error::into_backoff_error))
        .with_backoff(&mut backoff)
        .await
        .expect("should have succeeded");

    let re = regex::Regex::new(r"(\d+)-(\d+)/(\d+)").unwrap();
    let left = document
        .find(Attr("id", "listaHasel"))
        .next()
        .unwrap()
        .children()
        .nth(0)
        .unwrap()
        .text();

    let captures = re.captures(left.trim()).unwrap();
    (letter, captures.get(3).unwrap().as_str().parse().unwrap())
}

pub async fn get_def_with_backoff(ssid: String, id: u32) -> Result<String, Error> {
    use backoff_futures::TryFutureExt as _;
    use futures::future::TryFutureExt;
    use select::predicate::Attr;

    let mut backoff = backoff::ExponentialBackoff::default();
    let document =
        (|| get_page(&ssid, 'A', 0, Some(id)).map_err(Error::into_backoff_error))
            .with_backoff(&mut backoff)
            .await
            .expect("should have succeeded");

    document
        .find(Attr("id", "haslo"))
        .next()
        .ok_or(Error::MissingElement("id"))
        .map(|elem| elem.inner_html().trim().replace('\n', " "))
}

pub async fn get_words_from_page_with_backoff(ssid: String, letter: char, offset: u16) -> Vec<(u32, String)> {
    use backoff_futures::TryFutureExt as _;
    use futures::future::TryFutureExt;
    use select::predicate::{Attr, Child, Name};

    let re = regex::Regex::new(r"javascript: haslo\((\d+)").unwrap();

    let mut backoff = backoff::ExponentialBackoff::default();
    let document =
        (|| get_page(&ssid, letter, offset, None).map_err(Error::into_backoff_error))
            .with_backoff(&mut backoff)
            .await
            .expect("should have succeeded");

    let selection = document
        .find(Child(Attr("id", "listaHasel"), Name("div")))
        .into_selection();

    selection
        .iter()
        .take(selection.len() - 1)
        .map(|node| {
            let a = node
                .find(Name("a"))
                .find(|c| c.attr("id").is_none())
                .unwrap();
            let id: u32 = re
                .captures(a.attr("href").unwrap())
                .unwrap()
                .get(1)
                .unwrap()
                .as_str()
                .parse()
                .unwrap();
            (id, a.text())
        })
        .collect()
}

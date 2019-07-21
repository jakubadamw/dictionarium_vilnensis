#![feature(async_await)]

use hyper::{Client, Request, Uri};

#[derive(Debug)]
pub enum Error {
    Http(http::Error),
    Hyper(hyper::Error),
    Io(std::io::Error),
    Cookie(CookieHeaderError),
    String(std::string::FromUtf8Error)
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

pub async fn get_page(ssid: &str, letter: char, offset: u8, word: Option<u32>) -> Result<select::document::Document, Error> {
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
            cookie::Cookie::new("PHPSESSID", ssid.to_string()).to_string())
        .body(data.finish().into())?;

    let body = client
        .request(request)
        .and_then(|res| res.into_body().try_concat())
        .await?;

    Ok(select::document::Document::from_read(body.as_ref())?)
}

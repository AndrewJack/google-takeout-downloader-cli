use std::time::Duration;

use indicatif::ProgressBar;
use takeout_dl::cookies;
use takeout_dl::download::{DownloadError, Downloader, Outcome};
use takeout_dl::urls::Part;
use tempfile::TempDir;
use url::Url;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BODY: &[u8] = b"0123456789";
const FILE: &str = "takeout-1-001.tgz";

async fn setup() -> (MockServer, TempDir, Downloader, Part) {
    let server = MockServer::start().await;
    let dir = TempDir::new().unwrap();
    let jar = cookies::into_jar(
        &cookies::parse("127.0.0.1\tFALSE\t/\tFALSE\t0\tSID\tsecret\n", 0).unwrap(),
    );
    let dl = Downloader::new(jar, dir.path().to_path_buf(), 2)
        .unwrap()
        .with_base_backoff(Duration::from_millis(1));
    let part =
        Part::from_url(Url::parse(&format!("{}/download/{FILE}?i=0", server.uri())).unwrap())
            .unwrap();
    (server, dir, dl, part)
}

fn ok_body() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(BODY, "application/x-gzip")
}

#[tokio::test]
async fn full_download_sends_cookies() {
    let (server, dir, dl, part) = setup().await;
    Mock::given(method("GET"))
        .and(path(format!("/download/{FILE}")))
        .and(header("cookie", "SID=secret"))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let outcome = dl.download(&part, &ProgressBar::hidden()).await.unwrap();
    assert_eq!(outcome, Outcome::Downloaded);
    assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), BODY);
    assert!(!dir.path().join(format!("{FILE}.part")).exists());
}

#[tokio::test]
async fn resumes_partial_file_with_range() {
    let (server, dir, dl, part) = setup().await;
    std::fs::write(dir.path().join(format!("{FILE}.part")), &BODY[..4]).unwrap();
    Mock::given(method("GET"))
        .and(header("range", "bytes=4-"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 4-9/10")
                .set_body_raw(&BODY[4..], "application/x-gzip"),
        )
        .expect(1)
        .mount(&server)
        .await;

    dl.download(&part, &ProgressBar::hidden()).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), BODY);
}

#[tokio::test]
async fn restarts_when_range_ignored() {
    let (server, dir, dl, part) = setup().await;
    std::fs::write(dir.path().join(format!("{FILE}.part")), b"junk").unwrap();
    Mock::given(method("GET"))
        .and(header_exists("range"))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    dl.download(&part, &ProgressBar::hidden()).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), BODY);
}

#[tokio::test]
async fn login_redirect_is_auth_error_without_retry() {
    let (server, _dir, dl, part) = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            "https://accounts.google.com/ServiceLogin?continue=x",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let err = dl
        .download(&part, &ProgressBar::hidden())
        .await
        .unwrap_err();
    assert!(matches!(err, DownloadError::Auth), "{err:?}");
}

#[tokio::test]
async fn html_response_is_auth_error() {
    let (server, _dir, dl, part) = setup().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<html>sign in</html>", "text/html; charset=UTF-8"),
        )
        .mount(&server)
        .await;

    let err = dl
        .download(&part, &ProgressBar::hidden())
        .await
        .unwrap_err();
    assert!(matches!(err, DownloadError::Auth), "{err:?}");
}

#[tokio::test]
async fn retries_server_errors() {
    let (server, dir, dl, part) = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ok_body())
        .mount(&server)
        .await;

    dl.download(&part, &ProgressBar::hidden()).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), BODY);
}

#[tokio::test]
async fn gives_up_after_retry_budget() {
    let (server, _dir, dl, part) = setup().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(3)
        .mount(&server)
        .await;

    let err = dl
        .download(&part, &ProgressBar::hidden())
        .await
        .unwrap_err();
    assert!(matches!(err, DownloadError::Status(s) if s.as_u16() == 500));
}

#[tokio::test]
async fn skips_existing_file() {
    let (server, dir, dl, part) = setup().await;
    std::fs::write(dir.path().join(FILE), BODY).unwrap();
    Mock::given(method("GET"))
        .respond_with(ok_body())
        .expect(0)
        .mount(&server)
        .await;

    let outcome = dl.download(&part, &ProgressBar::hidden()).await.unwrap();
    assert_eq!(outcome, Outcome::AlreadyPresent);
}

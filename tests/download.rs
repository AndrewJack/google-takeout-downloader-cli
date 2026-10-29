use std::time::Duration;

use indicatif::ProgressBar;
use takeout_dl::cookies;

use takeout_dl::download::{DownloadError, Downloader, Outcome, Queue};
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

#[tokio::test]
async fn resume_sends_if_range_and_cleans_up() {
    let (server, dir, dl, part) = setup().await;
    let validator = dir.path().join(format!("{FILE}.part.validator"));
    std::fs::write(dir.path().join(format!("{FILE}.part")), &BODY[..4]).unwrap();
    std::fs::write(&validator, "\"v1\"").unwrap();
    Mock::given(method("GET"))
        .and(header("range", "bytes=4-"))
        .and(header("if-range", "\"v1\""))
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
    assert!(!validator.exists());
}

#[tokio::test]
async fn range_not_satisfiable_completes_part_of_matching_size() {
    let (server, dir, dl, part) = setup().await;
    std::fs::write(dir.path().join(format!("{FILE}.part")), BODY).unwrap();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(416).insert_header("content-range", "bytes */10"))
        .expect(1)
        .mount(&server)
        .await;

    let outcome = dl.download(&part, &ProgressBar::hidden()).await.unwrap();
    assert_eq!(outcome, Outcome::Downloaded);
    assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), BODY);
}

#[tokio::test]
async fn oversized_part_restarts_from_scratch() {
    let (server, dir, dl, part) = setup().await;
    std::fs::write(dir.path().join(format!("{FILE}.part")), b"0123456789xx").unwrap();
    Mock::given(method("GET"))
        .and(header_exists("range"))
        .respond_with(ResponseTemplate::new(416).insert_header("content-range", "bytes */10"))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    dl.download(&part, &ProgressBar::hidden()).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), BODY);
}

fn gzipped(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

#[tokio::test]
async fn verify_accepts_valid_gzip() {
    let (server, dir, dl, part) = setup().await;
    let body = gzipped(BODY);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.clone(), "application/x-gzip"))
        .mount(&server)
        .await;

    let outcome = dl
        .with_verify(true)
        .download(&part, &ProgressBar::hidden())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Downloaded);
    assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), body);
}

#[tokio::test]
async fn verify_moves_corrupt_file_aside_without_retry() {
    let (server, dir, dl, part) = setup().await;
    Mock::given(method("GET"))
        .respond_with(ok_body().insert_header("etag", "\"v1\""))
        .expect(1)
        .mount(&server)
        .await;

    let err = dl
        .with_verify(true)
        .download(&part, &ProgressBar::hidden())
        .await
        .unwrap_err();
    assert!(matches!(err, DownloadError::Corrupt { .. }), "{err:?}");
    let corrupt = dir.path().join(format!("{FILE}.corrupt"));
    assert_eq!(std::fs::read(corrupt).unwrap(), BODY);
    for leftover in [
        FILE.to_owned(),
        format!("{FILE}.part"),
        format!("{FILE}.part.validator"),
    ] {
        assert!(
            !dir.path().join(&leftover).exists(),
            "{leftover} left behind"
        );
    }
}

#[tokio::test]
async fn resume_offset_does_not_inflate_speed() {
    let (server, dir, dl, part) = setup().await;
    let have = 64 << 20;
    std::fs::write(dir.path().join(format!("{FILE}.part")), vec![0u8; have]).unwrap();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header(
                    "content-range",
                    format!("bytes {have}-{}/{}", have + 9, have + 10),
                )
                .set_body_raw(BODY, "application/x-gzip"),
        )
        .mount(&server)
        .await;

    let pb = ProgressBar::hidden();
    dl.download(&part, &pb).await.unwrap();
    // Only the 10 bytes actually transferred count towards the rate, not the 64 MiB on disk.
    assert!(pb.per_sec() < 1_000_000.0, "rate {} B/s", pb.per_sec());
}

#[tokio::test]
async fn queued_part_starts_at_once_then_speeds_up_when_its_turn_comes() {
    let (server, dir, dl, part) = setup().await;
    Mock::given(method("GET"))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;
    // At 1 B/s the 10-byte body would take 10 s to trickle.
    let dl = dl.with_trickle_rate(1);
    let queue = Queue::new(1);
    let ahead = queue.join();
    let pb = ProgressBar::hidden();
    let started = std::time::Instant::now();

    let (result, ()) = tokio::join!(dl.download_queued(&part, &pb, queue.join()), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        // The request went out before the part had a slot.
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert!(!dir.path().join(FILE).exists());
        drop(ahead);
    });
    assert_eq!(result.unwrap(), Outcome::Downloaded);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), BODY);
}

#[tokio::test]
async fn queued_part_finishes_by_trickling_if_its_turn_never_comes() {
    let (server, dir, dl, part) = setup().await;
    Mock::given(method("GET"))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;
    let queue = Queue::new(1);
    let _ahead = queue.join();

    let outcome = dl
        .download_queued(&part, &ProgressBar::hidden(), queue.join())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Downloaded);
    assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), BODY);
}

#[tokio::test]
async fn slots_go_to_parts_in_queue_order() {
    let (server, _dir, dl, part) = setup().await;
    Mock::given(method("GET"))
        .respond_with(ok_body())
        .mount(&server)
        .await;
    // At 1 B/s neither part can finish by trickling within the test.
    let dl = dl.with_trickle_rate(1);
    let pb = ProgressBar::hidden();
    let later = Part::from_url(
        Url::parse(&format!("{}/download/takeout-1-002.tgz?i=1", server.uri())).unwrap(),
    )
    .unwrap();
    let queue = Queue::new(1);
    let ahead = queue.join();
    let (first, second) = (queue.join(), queue.join());
    let done = std::sync::Mutex::new(Vec::new());
    let run = |name: &'static str, p, turn, delay| {
        let (dl, pb, done) = (&dl, &pb, &done);
        async move {
            tokio::time::sleep(delay).await;
            dl.download_queued(p, pb, turn).await.unwrap();
            done.lock().unwrap().push(name);
        }
    };

    let started = std::time::Instant::now();
    tokio::join!(
        // The second part's download starts first, but its place in the queue is still second.
        run("second", &later, second, Duration::ZERO),
        run("first", &part, first, Duration::from_millis(50)),
        async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(ahead);
        }
    );
    // One slot: the first part used it, then handed it to the second.
    assert_eq!(*done.lock().unwrap(), ["first", "second"]);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

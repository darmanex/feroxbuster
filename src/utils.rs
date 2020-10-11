use crate::config::CONFIGURATION;
use crate::FeroxResult;
use console::{strip_ansi_codes, style, user_attended};
use indicatif::ProgressBar;
use reqwest::Url;
use reqwest::{Client, Response};
use std::convert::TryInto;
use std::io::Write;
use std::sync::{Arc, RwLock};
use std::{fs, io};

/// Given the path to a file, open the file in append mode (create it if it doesn't exist) and
/// return a reference to the file that is buffered and locked
pub fn open_file(filename: &str) -> Option<Arc<RwLock<io::BufWriter<fs::File>>>> {
    log::trace!("enter: open_file({})", filename);

    match fs::OpenOptions::new() // std fs
        .create(true)
        .append(true)
        .open(filename)
    {
        Ok(file) => {
            let writer = io::BufWriter::new(file); // std io

            let locked_file = Some(Arc::new(RwLock::new(writer)));

            log::trace!("exit: open_file -> {:?}", locked_file);
            locked_file
        }
        Err(e) => {
            log::error!("{}", e);
            log::trace!("exit: open_file -> None");
            None
        }
    }
}

/// Given a string and a reference to a locked buffered file, write the contents and flush
/// the buffer to disk.
pub fn safe_file_write(contents: &str, locked_file: Arc<RwLock<io::BufWriter<fs::File>>>) {
    log::trace!("enter: safe_file_write({}, {:?})", contents, locked_file);

    let contents = strip_ansi_codes(&contents);

    if let Ok(mut handle) = locked_file.write() {
        // write lock acquired
        match handle.write(contents.as_bytes()) {
            Ok(written) => {
                log::trace!("wrote {} bytes to {}", written, CONFIGURATION.output);
            }
            Err(e) => {
                log::error!("could not write report to disk: {}", e);
            }
        }

        match handle.flush() {
            // this function is used within async functions/loops, so i'm flushing so that in
            // the event of a ctrl+c or w/e results seen so far are saved instead of left lying
            // around in the buffer
            Ok(_) => {}
            Err(e) => {
                log::error!("error writing to file: {}", e);
            }
        }
    }

    log::trace!("exit: safe_file_write");
}

/// Helper function that determines the current depth of a given url
///
/// Essentially looks at the Url path and determines how many directories are present in the
/// given Url
///
/// http://localhost -> 1
/// http://localhost/ -> 1
/// http://localhost/stuff -> 2
/// ...
///
/// returns 0 on error and relative urls
pub fn get_current_depth(target: &str) -> usize {
    log::trace!("enter: get_current_depth({})", target);

    let target = if !target.ends_with('/') {
        // target url doesn't end with a /, for the purposes of determining depth, we'll normalize
        // all urls to end in a / and then calculate accordingly
        format!("{}/", target)
    } else {
        String::from(target)
    };

    match Url::parse(&target) {
        Ok(url) => {
            if let Some(parts) = url.path_segments() {
                // at least an empty string returned by the Split, meaning top-level urls
                let mut depth = 0;

                for _ in parts {
                    depth += 1;
                }

                let return_val = depth;

                log::trace!("exit: get_current_depth -> {}", return_val);
                return return_val;
            };

            log::debug!(
                "get_current_depth called on a Url that cannot be a base: {}",
                url
            );
            log::trace!("exit: get_current_depth -> 0");

            0
        }
        Err(e) => {
            log::error!("could not parse to url: {}", e);
            log::trace!("exit: get_current_depth -> 0");
            0
        }
    }
}

/// Takes in a string and examines the first character to return a color version of the same string
pub fn status_colorizer(status: &str) -> String {
    match status.chars().next() {
        Some('1') => style(status).blue().to_string(), // informational
        Some('2') => style(status).green().to_string(), // success
        Some('3') => style(status).yellow().to_string(), // redirects
        Some('4') => style(status).red().to_string(),  // client error
        Some('5') => style(status).red().to_string(),  // server error
        Some('W') => style(status).cyan().to_string(), // wildcard
        Some('E') => style(status).red().to_string(),  // error
        _ => status.to_string(),                       // ¯\_(ツ)_/¯
    }
}

/// Takes in a string and colors it using console::style
///
/// mainly putting this here in case i want to change the color later, making any changes easy
pub fn module_colorizer(modname: &str) -> String {
    style(modname).cyan().to_string()
}

/// Gets the length of a url's path
///
/// example: http://localhost/stuff -> 5
pub fn get_url_path_length(url: &Url) -> u64 {
    log::trace!("enter: get_url_path_length({})", url);

    let path = url.path();

    let segments = if path.starts_with('/') {
        path[1..].split_terminator('/')
    } else {
        log::trace!("exit: get_url_path_length -> 0");
        return 0;
    };

    if let Some(last) = segments.last() {
        // failure on conversion should be very unlikely. While a usize can absolutely overflow a
        // u64, the generally accepted maximum for the length of a url is ~2000.  so the value we're
        // putting into the u64 should never realistically be anywhere close to producing an
        // overflow.
        // usize max: 18,446,744,073,709,551,615
        // u64 max:   9,223,372,036,854,775,807
        let url_len: u64 = last
            .len()
            .try_into()
            .expect("Failed usize -> u64 conversion");

        log::trace!("exit: get_url_path_length -> {}", url_len);
        return url_len;
    }

    log::trace!("exit: get_url_path_length -> 0");
    0
}

/// Simple helper to abstract away the check for an attached terminal.
///
/// If a terminal is attached, progress bars are visible and the progress bar is used to print
/// to stderr. The progress bar must be used when bars are visible in order to not jack up any
/// progress bar output (the bar knows how to print above itself)
///
/// If a terminal is not attached, `msg` is printed to stdout, with its ansi
/// color codes stripped.
///
/// additionally, provides a location for future printing options (no color, etc) to be handled
pub fn ferox_print(msg: &str, bar: &ProgressBar) {
    if user_attended() {
        bar.println(msg);
    } else {
        let stripped = strip_ansi_codes(msg);
        println!("{}", stripped);
    }
}

/// Simple helper to generate a `Url`
///
/// Errors during parsing `url` or joining `word` are propagated up the call stack
pub fn format_url(
    url: &str,
    word: &str,
    addslash: bool,
    queries: &[(String, String)],
    extension: Option<&str>,
) -> FeroxResult<Url> {
    log::trace!(
        "enter: format_url({}, {}, {}, {:?} {:?})",
        url,
        word,
        addslash,
        queries,
        extension
    );

    // from reqwest::Url::join
    //   Note: a trailing slash is significant. Without it, the last path component
    //   is considered to be a “file” name to be removed to get at the “directory”
    //   that is used as the base
    //
    // the transforms that occur here will need to keep this in mind, i.e. add a slash to preserve
    // the current directory sent as part of the url
    let url = if !url.ends_with('/') {
        format!("{}/", url)
    } else {
        url.to_string()
    };

    let base_url = reqwest::Url::parse(&url)?;

    // extensions and slashes are mutually exclusive cases
    let word = if extension.is_some() {
        format!("{}.{}", word, extension.unwrap())
    } else if addslash && !word.ends_with('/') {
        // -f used, and word doesn't already end with a /
        format!("{}/", word)
    } else {
        String::from(word)
    };

    match base_url.join(&word) {
        Ok(request) => {
            if queries.is_empty() {
                // no query params to process
                log::trace!("exit: format_url -> {}", request);
                Ok(request)
            } else {
                match reqwest::Url::parse_with_params(request.as_str(), queries) {
                    Ok(req_w_params) => {
                        log::trace!("exit: format_url -> {}", req_w_params);
                        Ok(req_w_params) // request with params attached
                    }
                    Err(e) => {
                        log::error!(
                            "Could not add query params {:?} to {}: {}",
                            queries,
                            request,
                            e
                        );
                        log::trace!("exit: format_url -> {}", request);
                        Ok(request) // couldn't process params, return initially ok url
                    }
                }
            }
        }
        Err(e) => {
            log::trace!("exit: format_url -> {}", e);
            log::error!("Could not join {} with {}", word, base_url);
            Err(Box::new(e))
        }
    }
}

/// Initiate request to the given `Url` using `Client`
pub async fn make_request(client: &Client, url: &Url) -> FeroxResult<Response> {
    log::trace!("enter: make_request(CONFIGURATION.Client, {})", url);

    match client.get(url.to_owned()).send().await {
        Ok(resp) => {
            log::debug!("requested Url: {}", resp.url());
            log::trace!("exit: make_request -> {:?}", resp);
            Ok(resp)
        }
        Err(e) => {
            log::trace!("exit: make_request -> {}", e);
            if e.to_string().contains("operation timed out") {
                // only warn for timeouts, while actual errors are still left as errors
                log::warn!("Error while making request: {}", e);
            } else {
                log::error!("Error while making request: {}", e);
            }
            Err(Box::new(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// base url returns 1
    fn get_current_depth_base_url_returns_1() {
        let depth = get_current_depth("http://localhost");
        assert_eq!(depth, 1);
    }

    #[test]
    /// base url with slash returns 1
    fn get_current_depth_base_url_with_slash_returns_1() {
        let depth = get_current_depth("http://localhost/");
        assert_eq!(depth, 1);
    }

    #[test]
    /// base url + 1 dir returns 2
    fn get_current_depth_one_dir_returns_2() {
        let depth = get_current_depth("http://localhost/src");
        assert_eq!(depth, 2);
    }

    #[test]
    /// base url + 1 dir and slash returns 2
    fn get_current_depth_one_dir_with_slash_returns_2() {
        let depth = get_current_depth("http://localhost/src/");
        assert_eq!(depth, 2);
    }

    #[test]
    /// base url + 1 dir and slash returns 2
    fn get_current_depth_single_forward_slash_is_zero() {
        let depth = get_current_depth("");
        assert_eq!(depth, 0);
    }

    #[test]
    /// base url + 1 word + no slash + no extension
    fn format_url_normal() {
        assert_eq!(
            format_url("http://localhost", "stuff", false, &Vec::new(), None).unwrap(),
            reqwest::Url::parse("http://localhost/stuff").unwrap()
        );
    }

    #[test]
    /// base url + no word + no slash + no extension
    fn format_url_no_word() {
        assert_eq!(
            format_url("http://localhost", "", false, &Vec::new(), None).unwrap(),
            reqwest::Url::parse("http://localhost").unwrap()
        );
    }

    #[test]
    /// base url + word + no slash + no extension + queries
    fn format_url_joins_queries() {
        assert_eq!(
            format_url(
                "http://localhost",
                "lazer",
                false,
                &[(String::from("stuff"), String::from("things"))],
                None
            )
            .unwrap(),
            reqwest::Url::parse("http://localhost/lazer?stuff=things").unwrap()
        );
    }

    #[test]
    /// base url + no word + no slash + no extension + queries
    fn format_url_without_word_joins_queries() {
        assert_eq!(
            format_url(
                "http://localhost",
                "",
                false,
                &[(String::from("stuff"), String::from("things"))],
                None
            )
            .unwrap(),
            reqwest::Url::parse("http://localhost/?stuff=things").unwrap()
        );
    }

    #[test]
    #[should_panic]
    /// no base url is an error
    fn format_url_no_url() {
        format_url("", "stuff", false, &Vec::new(), None).unwrap();
    }

    #[test]
    /// word prepended with slash is adjusted for correctness
    fn format_url_word_with_preslash() {
        assert_eq!(
            format_url("http://localhost", "/stuff", false, &Vec::new(), None).unwrap(),
            reqwest::Url::parse("http://localhost/stuff").unwrap()
        );
    }

    #[test]
    /// word with appended slash allows the slash to persist
    fn format_url_word_with_postslash() {
        assert_eq!(
            format_url("http://localhost", "stuff/", false, &Vec::new(), None).unwrap(),
            reqwest::Url::parse("http://localhost/stuff/").unwrap()
        );
    }

    #[test]
    /// status colorizer uses red for 500s
    fn status_colorizer_uses_red_for_500s() {
        assert_eq!(status_colorizer("500"), style("500").red().to_string());
    }

    #[test]
    /// status colorizer uses red for 400s
    fn status_colorizer_uses_red_for_400s() {
        assert_eq!(status_colorizer("400"), style("400").red().to_string());
    }

    #[test]
    /// status colorizer uses red for errors
    fn status_colorizer_uses_red_for_errors() {
        assert_eq!(status_colorizer("ERROR"), style("ERROR").red().to_string());
    }

    #[test]
    /// status colorizer uses cyan for wildcards
    fn status_colorizer_uses_cyan_for_wildcards() {
        assert_eq!(status_colorizer("WLD"), style("WLD").cyan().to_string());
    }

    #[test]
    /// status colorizer uses blue for 100s
    fn status_colorizer_uses_blue_for_100s() {
        assert_eq!(status_colorizer("100"), style("100").blue().to_string());
    }

    #[test]
    /// status colorizer uses green for 200s
    fn status_colorizer_uses_green_for_200s() {
        assert_eq!(status_colorizer("200"), style("200").green().to_string());
    }

    #[test]
    /// status colorizer uses yellow for 300s
    fn status_colorizer_uses_yellow_for_300s() {
        assert_eq!(status_colorizer("300"), style("300").yellow().to_string());
    }

    #[test]
    /// status colorizer doesnt color anything else
    fn status_colorizer_returns_as_is() {
        assert_eq!(status_colorizer("farfignewton"), "farfignewton".to_string());
    }
}

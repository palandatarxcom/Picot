// ABOUTME: Official CRAN mirror selection for R downloads: the first OK
// ABOUTME mainland-China entry from CRAN's own mirror list. Deterministic by
// ABOUTME design — never a speed test, never a hand-curated list.

/// The authorization entry for R in every prompt, mirror or no mirror.
pub(crate) const CRAN_OFFICIAL_URL: &str = "https://cran.r-project.org/";

const CRAN_MIRRORS_CSV_URL: &str = "https://cran.r-project.org/CRAN_mirrors.csv";
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Resolve the download entry for installing R: the first `OK==1`
/// `CountryCode==cn` mirror in CRAN's official list, falling back to the
/// official root when the list cannot be fetched or parsed.
pub(crate) async fn select_r_download_url() -> String {
    match fetch_official_csv().await {
        Some(csv) => first_ok_cn_mirror(&csv).unwrap_or(CRAN_OFFICIAL_URL.to_string()),
        None => CRAN_OFFICIAL_URL.to_string(),
    }
}

/// One bounded GET of CRAN's mirror list; any failure is a fallback.
async fn fetch_official_csv() -> Option<String> {
    let response = reqwest::Client::new()
        .get(CRAN_MIRRORS_CSV_URL)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    response.text().await.ok()
}

/// The first mainland-China mirror CRAN itself marks healthy. Pure, so the
/// real CSV shape is testable without network.
fn first_ok_cn_mirror(csv: &str) -> Option<String> {
    let mut rows = csv.lines().filter(|line| !line.trim().is_empty());
    let header = parse_row(rows.next()?);
    let url_at = header.iter().position(|field| field == "URL")?;
    let ok_at = header.iter().position(|field| field == "OK")?;
    let code_at = header.iter().position(|field| field == "CountryCode")?;
    for line in rows {
        let fields = parse_row(line);
        if fields.get(code_at).map(String::as_str) != Some("cn") {
            continue;
        }
        if fields.get(ok_at).map(String::as_str) != Some("1") {
            continue;
        }
        if let Some(url) = fields.get(url_at).map(String::as_str) {
            let url = url.trim();
            // Download entry stays on https; the list only carries https
            // mirrors, so anything else is malformed input.
            if url.starts_with("https://") {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// Split one quoted CSV record. The mirror list quotes every field and does
/// not embed newlines, so one line is one record.
fn parse_row(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        if in_quotes {
            if character == '"' {
                if characters.peek() == Some(&'"') {
                    current.push('"');
                    characters.next();
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(character);
            }
        } else {
            match character {
                '"' => in_quotes = true,
                ',' => fields.push(std::mem::take(&mut current)),
                _ => current.push(character),
            }
        }
    }
    fields.push(current);
    fields
}

#[cfg(test)]
mod mirror_tests {
    use super::*;

    /// Trimmed from https://cran.r-project.org/CRAN_mirrors.csv (2026-11-11):
    /// the header and the first mainland-China rows in their real order.
    const REAL_SAMPLE: &str = r#""Name","Country","City","URL","Host","Maintainer","OK","CountryCode","Comment"
"China (Beijing 1) [https]","China","Beijing","https://mirrors.tuna.tsinghua.edu.cn/CRAN/","TUNA Team, Tsinghua University","Justin Wong",1,"cn","secure_mirror_from_master"
"China (Beijing 2) [https]","China","Beijing","https://mirrors.bfsu.edu.cn/CRAN/","Beijing Foreign Studies University","Aron Xu",1,"cn","secure_mirror_from_master"
"China (Chongqing) [https]","China","Chongqing","https://mirrors.cqu.edu.cn/CRAN/","Chongqing University","cqumirror",0,"cn",""
"China (Hefei) [https]","China","Hefei","https://mirrors.ustc.edu.cn/CRAN/","University of Science and Technology of China","mirrors",1,"cn","secure_mirror_from_master"
"Austria [https]","Austria","Vienna","https://cran.r-project.org/","R Foundation","mirror@r-project.org",1,"at",""
"#;

    #[test]
    fn picks_the_first_healthy_china_mirror_in_list_order() {
        assert_eq!(
            first_ok_cn_mirror(REAL_SAMPLE).as_deref(),
            Some("https://mirrors.tuna.tsinghua.edu.cn/CRAN/")
        );
    }

    #[test]
    fn skips_a_china_mirror_that_cran_marks_unhealthy() {
        // Remove the two Beijing OK mirrors: Chongqing (OK=0) must not be
        // picked even though it comes first among the remaining cn rows.
        let csv = REAL_SAMPLE
            .lines()
            .filter(|line| !line.contains("tuna.tsinghua") && !line.contains("bfsu"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            first_ok_cn_mirror(&csv).as_deref(),
            Some("https://mirrors.ustc.edu.cn/CRAN/")
        );
    }

    #[test]
    fn a_row_with_a_comma_inside_quotes_still_parses() {
        // "TUNA Team, Tsinghua University" carries a comma inside quotes.
        let csv = "Name,Country,City,URL,Host,Maintainer,OK,CountryCode,Comment\n\"X\",\"China\",\"Beijing\",\"https://example.cn/CRAN/\",\"A, B\",\"someone\",1,\"cn\",\"\n";
        assert_eq!(
            first_ok_cn_mirror(csv).as_deref(),
            Some("https://example.cn/CRAN/")
        );
    }

    #[test]
    fn without_a_healthy_china_mirror_there_is_no_pick() {
        let no_cn = "Name,Country,City,URL,Host,Maintainer,OK,CountryCode,Comment\n\"Austria [https]\",\"Austria\",\"Vienna\",\"https://cran.r-project.org/\",\"R Foundation\",\"x\",1,\"at\",\"\n";
        assert_eq!(first_ok_cn_mirror(no_cn), None);
        let cn_unhealthy = "Name,Country,City,URL,Host,Maintainer,OK,CountryCode,Comment\n\"X\",\"China\",\"Beijing\",\"https://example.cn/CRAN/\",\"A\",\"s\",0,\"cn\",\"\n";
        assert_eq!(first_ok_cn_mirror(cn_unhealthy), None);
        assert_eq!(first_ok_cn_mirror(""), None);
        assert_eq!(first_ok_cn_mirror("not csv at all"), None);
    }

    #[test]
    fn a_non_https_or_malformed_url_is_rejected() {
        let csv = "Name,Country,City,URL,Host,Maintainer,OK,CountryCode,Comment\n\"X\",\"China\",\"Beijing\",\"http://insecure.example/CRAN/\",\"A\",\"s\",1,\"cn\",\"\n";
        assert_eq!(first_ok_cn_mirror(csv), None);
    }
}

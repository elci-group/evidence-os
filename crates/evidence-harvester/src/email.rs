//! `.eml` parsing via mailparse: RFC2822 date, From/To/Cc, Subject and
//! multipart attachments (any part carrying a filename, attachment or inline).

use anyhow::Context;
use chrono::DateTime;
use mailparse::{addrparse, MailAddr, MailHeaderMap, ParsedMail};

/// What we could pull out of one email file.
pub(crate) struct ParsedEmail {
    pub date: Option<String>,
    pub sender: Option<String>,
    pub recipients: Option<String>,
    pub subject: Option<String>,
    pub attachments: Vec<EmailAttachment>,
}

/// One decoded attachment (Content-Transfer-Encoding already undone).
pub(crate) struct EmailAttachment {
    pub filename: String,
    pub data: Vec<u8>,
}

/// Parse raw `.eml` bytes. Fails only when the message is not parseable as
/// email at all; individual missing headers yield None fields.
pub(crate) fn parse_eml(bytes: &[u8]) -> anyhow::Result<ParsedEmail> {
    let mail = mailparse::parse_mail(bytes).context("parsing .eml file")?;
    let headers = &mail.headers;
    let date = headers
        .get_first_value("Date")
        .and_then(|d| rfc2822_to_iso(&d));
    let sender = headers
        .get_first_value("From")
        .and_then(|v| first_display_name(&v));
    let recipients = recipients(&mail);
    let subject = headers
        .get_first_value("Subject")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut attachments = Vec::new();
    collect_attachments(&mail, &mut attachments);
    Ok(ParsedEmail {
        date,
        sender,
        recipients,
        subject,
        attachments,
    })
}

/// RFC2822 `Date` header -> ISO yyyy-mm-dd. Falls back to mailparse's
/// dateparse (epoch seconds) for obsolete two-digit-year formats.
fn rfc2822_to_iso(header: &str) -> Option<String> {
    if let Ok(dt) = DateTime::parse_from_rfc2822(header.trim()) {
        return Some(dt.format("%Y-%m-%d").to_string());
    }
    let epoch = mailparse::dateparse(header.trim()).ok()?;
    DateTime::from_timestamp(epoch, 0).map(|dt| dt.format("%Y-%m-%d").to_string())
}

/// Display name (or bare address) of the first mailbox in a header value.
fn first_display_name(value: &str) -> Option<String> {
    let parsed = addrparse(value).ok()?;
    display_names(&parsed).into_iter().next()
}

/// To + Cc, each mailbox rendered as display name or address, joined ", ".
fn recipients(mail: &ParsedMail) -> Option<String> {
    let mut names = Vec::new();
    for key in ["To", "Cc"] {
        if let Some(v) = mail.headers.get_first_value(key) {
            if let Ok(addrs) = addrparse(&v) {
                names.extend(display_names(&addrs));
            }
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

fn display_names(addrs: &[MailAddr]) -> Vec<String> {
    let mut out = Vec::new();
    for a in addrs {
        match a {
            MailAddr::Single(s) => out.push(s.display_name.clone().unwrap_or_else(|| s.addr.clone())),
            MailAddr::Group(g) => {
                for s in &g.addrs {
                    out.push(s.display_name.clone().unwrap_or_else(|| s.addr.clone()));
                }
            }
        }
    }
    out
}

/// Recursively collect every non-multipart part that carries a filename
/// (Content-Disposition filename or Content-Type name). Embedded inline
/// images count as evidence too.
fn collect_attachments(mail: &ParsedMail, out: &mut Vec<EmailAttachment>) {
    if mail.ctype.mimetype.starts_with("multipart/") {
        for part in &mail.subparts {
            collect_attachments(part, out);
        }
        return;
    }
    let disposition = mail.get_content_disposition();
    let filename = disposition
        .params
        .get("filename")
        .or_else(|| mail.ctype.params.get("name"))
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty());
    if let Some(filename) = filename {
        if let Ok(data) = mail.get_body_raw() {
            out.push(EmailAttachment { filename, data });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/covering-email.eml");

    #[test]
    fn parses_headers_date_and_single_attachment() {
        let bytes = std::fs::read(FIXTURE).unwrap();
        let parsed = parse_eml(&bytes).unwrap();
        assert_eq!(parsed.date.as_deref(), Some("2026-07-03"));
        assert_eq!(parsed.sender.as_deref(), Some("Sally-Ann Spring"));
        let recipients = parsed.recipients.unwrap();
        assert!(recipients.contains("KGS Solicitors"), "to: {recipients}");
        assert!(recipients.contains("john@example.net"), "cc: {recipients}");
        assert_eq!(
            parsed.subject.as_deref(),
            Some("Covering email supplying CrowdJustice contact details")
        );
        assert_eq!(parsed.attachments.len(), 1);
        let att = &parsed.attachments[0];
        assert_eq!(att.filename, "crowdjustice-contacts.txt");
        assert_eq!(
            String::from_utf8(att.data.clone()).unwrap(),
            "CrowdJustice contacts: jane@example.org, 01234 567890\n"
        );
    }
}

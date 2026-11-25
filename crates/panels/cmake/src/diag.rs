//! Errors and warnings in build output: MSVC and nvcc (`file(line[,col]): error
//! C2065: …`), clang and gcc (`file:line:col: error: …`), CMake (`CMake Error at
//! file:line (command):`) and linkers (`LINK : fatal error LNK1104: …`).

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diag {
    pub severity: Severity,
    /// The `file:line` part of the output line (to link), if it has one.
    pub range: Option<Range<usize>>,
    pub path: Option<String>,
    /// 1-based.
    pub line: u32,
    pub col: u32,
    /// What it says, after the severity and code.
    pub message: String,
}

fn severity(s: &str) -> Severity {
    let s = s.to_ascii_lowercase();
    if s.contains("error") {
        Severity::Error
    } else if s.contains("warning") {
        Severity::Warning
    } else {
        Severity::Note
    }
}

// A path: an optional drive, then no `(`, `:` or line break.
static MSVC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?P<loc>(?P<path>(?:[A-Za-z]:)?[^(:\r\n]+?)\((?P<line>\d+)(?:,(?P<col>\d+))?\))\s*:\s*(?P<sev>fatal error|error|warning|note|remark)\b(?:\s*(?:[A-Z]+\d+|#\d+-D))?\s*:?\s*(?P<msg>.*)$",
    )
    .unwrap()
});
static GCC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?P<loc>(?P<path>(?:[A-Za-z]:)?[^:\r\n]+):(?P<line>\d+)(?::(?P<col>\d+))?):\s*(?P<sev>fatal error|error|warning|note)\s*:\s*(?P<msg>.*)$",
    )
    .unwrap()
});
static CMAKE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^CMake (?P<sev>Error|Warning)(?: \(dev\))? at (?P<loc>(?P<path>(?:[A-Za-z]:)?[^:\r\n]+):(?P<line>\d+))(?:\s*\((?P<msg>[^)]*)\))?",
    )
    .unwrap()
});
static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:.*?\s*:\s*)?(?P<sev>fatal error|error|warning) (?P<code>LNK\d+|MSB\d+)\s*:\s*(?P<msg>.*)$")
        .unwrap()
});
static LLD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:lld-link|ld(?:\.lld)?|ld64\.lld|collect2): (?P<sev>error|warning): (?P<msg>.*)$").unwrap()
});

/// The diagnostic in one line of output, if there is one.
pub fn parse(line: &str) -> Option<Diag> {
    for re in [&*MSVC, &*GCC, &*CMAKE] {
        if let Some(c) = re.captures(line) {
            let path = c.name("path")?.as_str().trim();
            // `http://…`, times (`12:30:01`) and the like aren't files.
            if path.contains("://") || path.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            let loc = c.name("loc")?;
            let lead = loc.as_str().len() - loc.as_str().trim_start().len();
            return Some(Diag {
                severity: severity(c.name("sev")?.as_str()),
                range: Some(loc.start() + lead..loc.end()),
                path: Some(path.to_string()),
                line: c.name("line")?.as_str().parse().ok()?,
                col: c.name("col").and_then(|m| m.as_str().parse().ok()).unwrap_or(1),
                message: c.name("msg").map(|m| m.as_str().trim().to_string()).unwrap_or_default(),
            });
        }
    }
    for re in [&*LINK, &*LLD] {
        if let Some(c) = re.captures(line) {
            let code = c.name("code").map(|m| format!("{}: ", m.as_str())).unwrap_or_default();
            return Some(Diag {
                severity: severity(c.name("sev")?.as_str()),
                range: None,
                path: None,
                line: 1,
                col: 1,
                message: format!("{code}{}", c.name("msg")?.as_str().trim()),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn msvc_and_nvcc() {
        let l = r"D:\vrg\vrgAr\Ar\CameraEnumerator.cpp(36,49): error C2039: 'Instance': is not a member of 'MfVideoCaptureHandle'";
        let d = parse(l).unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.path.as_deref(), Some(r"D:\vrg\vrgAr\Ar\CameraEnumerator.cpp"));
        assert_eq!((d.line, d.col), (36, 49));
        assert_eq!(&l[d.range.unwrap()], r"D:\vrg\vrgAr\Ar\CameraEnumerator.cpp(36,49)");
        assert_eq!(d.message, "'Instance': is not a member of 'MfVideoCaptureHandle'");

        let d = parse(r"D:\x\VideoCamera.h(10): fatal error C1083: Cannot open include file: 'atlbase.h': No such file or directory").unwrap();
        assert_eq!((d.severity, d.line, d.col), (Severity::Error, 10, 1));
        assert!(d.message.starts_with("Cannot open include file"));

        let d = parse(r"D:\x\k.cu(12): warning #177-D: variable was declared but never referenced").unwrap();
        assert_eq!((d.severity, d.line), (Severity::Warning, 12));
        assert_eq!(d.message, "variable was declared but never referenced");

        let d = parse(r"D:\x\a.cpp(5): note: see declaration of 'f'").unwrap();
        assert_eq!(d.severity, Severity::Note);
    }

    #[::core::prelude::v1::test]
    fn clang_and_gcc() {
        let d = parse("../../src/main.cpp:7:3: error: use of undeclared identifier 'x'").unwrap();
        assert_eq!(d.path.as_deref(), Some("../../src/main.cpp"));
        assert_eq!((d.line, d.col), (7, 3));
        let d = parse("D:/proj/src/a.cpp:12:5: warning: unused variable 'y' [-Wunused-variable]").unwrap();
        assert_eq!(d.path.as_deref(), Some("D:/proj/src/a.cpp"));
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(parse("/usr/include/x.h:3: error: bad").unwrap().line, 3);
    }

    #[::core::prelude::v1::test]
    fn cmake_and_linkers() {
        let d = parse("CMake Error at CMakeLists.txt:10 (add_executable):").unwrap();
        assert_eq!((d.path.as_deref(), d.line), (Some("CMakeLists.txt"), 10));
        assert_eq!(d.message, "add_executable");
        let d = parse("CMake Warning (dev) at D:/p/cmake/x.cmake:3 (message):").unwrap();
        assert_eq!((d.severity, d.path.as_deref()), (Severity::Warning, Some("D:/p/cmake/x.cmake")));

        let d = parse("LINK : fatal error LNK1104: cannot open file 'bin\\app.exe'").unwrap();
        assert_eq!((d.severity, d.path.clone()), (Severity::Error, None));
        assert_eq!(d.message, "LNK1104: cannot open file 'bin\\app.exe'");
        let d = parse("main.obj : error LNK2019: unresolved external symbol f").unwrap();
        assert!(d.message.starts_with("LNK2019"));
        assert_eq!(parse("lld-link: error: undefined symbol: f").unwrap().severity, Severity::Error);
    }

    #[::core::prelude::v1::test]
    fn ordinary_lines_are_not_diagnostics() {
        for l in [
            "[12/340] Building CXX object vrgAr/CMakeFiles/vrgAr.dir/Ar/Camera.cpp.obj",
            "-- Configuring done (1.2s)",
            "Build finished: 0 errors, 0 warnings",
            "at 12:30:01 done",
            "see https://example.com:443/a",
            "",
        ] {
            assert_eq!(parse(l), None, "{l}");
        }
    }
}

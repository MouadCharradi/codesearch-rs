// Copyright 2015 Vernon Jones.
// Original code Copyright 2011 The Go Authors.  All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.


extern crate bytecount;
#[macro_use]
extern crate clap;
extern crate grep;
#[macro_use]
extern crate log;
extern crate libc;
extern crate memchr;
extern crate regex;
extern crate regex_syntax;
extern crate termcolor;

extern crate consts;
extern crate libcustomlogger;
extern crate libcsearch;
extern crate libvarint;

use libcsearch::reader::IndexReader;
use libcsearch::regexp::{RegexInfo, Query};

use std::fs::File;
use std::io::{Read, Write};
use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};

use grep::{GrepBuilder, Grep};
use regex::bytes;
use regex::Regex;
use termcolor::{Color, ColorChoice, ColorSpec, Stdout, WriteColor};


#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PrintFormat {
    Normal,
    VisualStudio,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LinePart {
    Path,
    LineNumber,
    Separator,
    Match,
}

#[derive(Debug)]
pub struct MatchOptions {
    pub pattern: Regex,
    pub print_format: PrintFormat,
    pub print_count: bool,
    pub ignore_case: bool,
    pub files_with_matches_only: bool,
    pub line_number: bool,
    pub with_color: bool,
    pub max_count: Option<usize>,
}

const ABOUT: &'static str = "
Csearch behaves like grep over all indexed files, searching for regexp,
an RE2 (nearly PCRE) regular expression.

Csearch relies on the existence of an up-to-date index created ahead of time.
To build or rebuild the index that csearch uses, run:

	cindex path...

where path... is a list of directories or individual files to be included in the index.
If no index exists, this command creates one.  If an index already exists, cindex
overwrites it.  Run cindex --help for more.

Csearch uses the index stored in $CSEARCHINDEX or, if that variable is unset or
empty, $HOME/.csearchindex.
";


#[cfg(windows)]
const STDOUT_FILENO: i32 = 1;
#[cfg(not(windows))]
const STDOUT_FILENO: i32 = libc::STDOUT_FILENO as i32;

pub fn is_color_output_available() -> bool {
    let isatty = unsafe { libc::isatty(STDOUT_FILENO) != 0 };
    if !isatty {
        return false;
    }
    let t = if let Ok(term) = env::var("TERM") {
        term
    } else {
        return false;
    };
    if t == "dumb" {
        return false;
    }
    return true;
}

fn main() {
    libcustomlogger::init(log::LogLevelFilter::Info).unwrap();

    let matches = clap::App::new("csearch")
        .version(&crate_version!()[..])
        .author("Vernon Jones <vernonrjones@gmail.com> (original code copyright 2011 the Go \
                 authors)")
        .about(ABOUT)
        .arg(clap::Arg::with_name("PATTERN")
            .help("a regular expression to search with")
            .required(true)
            .use_delimiter(false)
            .index(1))
        .arg(clap::Arg::with_name("count")
            .short("c")
            .long("count")
            .help("print only a count of matching lines per file"))
        .arg(clap::Arg::with_name("color")
            .long("color")
            .help("highlight matching strings")
            .overrides_with("nocolor")
            .hidden(!cfg!(feature = "color")))
        .arg(clap::Arg::with_name("nocolor")
            .long("nocolor")
            .help("don't highlight matching strings")
            .overrides_with("color")
            .hidden(!cfg!(feature = "color")))
        .arg(clap::Arg::with_name("FILE_PATTERN")
            .short("G")
            .long("file-search-regex")
            .help("limit search to filenames matching FILE_PATTERN")
            .takes_value(true))
        .arg(clap::Arg::with_name("ignore-case")
            .short("i")
            .long("ignore-case")
            .help("Match case insensitively"))
        .arg(clap::Arg::with_name("files-with-matches")
            .short("l")
            .long("files-with-matches")
            .help("Only print filenames that contain matches (don't print the matching lines)"))
        .arg(clap::Arg::with_name("line-number")
            .short("n")
            .long("line-number")
            .help("print line number with output lines"))
        .arg(clap::Arg::with_name("visual-studio-format")
            .long("format-vs")
            .help("print lines in a format that can be parsed by Visual Studio 2008"))
        .arg(clap::Arg::with_name("NUM")
            .short("m")
            .long("max-count")
            .takes_value(true)
            .help("stop after NUM matches"))
        .arg(clap::Arg::with_name("bruteforce")
            .long("brute")
            .help("brute force - search all files in the index"))
        .arg(clap::Arg::with_name("INDEX_FILE")
            .long("indexpath")
            .takes_value(true)
            .help("use specified INDEX_FILE as the index path. overrides $CSEARCHINDEX."))
        .get_matches();

    // possibly add ignore case flag to the pattern
    let ignore_case = matches.is_present("ignore-case");

    // get the pattern provided by the user
    let pattern = {
        let user_pattern = matches.value_of("PATTERN").expect("Failed to get PATTERN");

        let ignore_case_flag = if ignore_case { "(?i)" } else { "" };
        let multiline_flag = "(?m)";
        String::from(ignore_case_flag) + multiline_flag + user_pattern
    };
    let regex_pattern = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(e) => panic!("PATTERN: {}", e),
    };

    // possibly override the csearchindex
    matches.value_of("INDEX_FILE").map(|p| {
        env::set_var("CSEARCHINDEX", p);
    });

    // combine cmdline options used for matching/output into a structure
    let match_options = MatchOptions {
        pattern: regex_pattern,
        print_format: if matches.is_present("visual-studio-format") {
            PrintFormat::VisualStudio
        } else {
            PrintFormat::Normal
        },
        print_count: matches.is_present("count"),
        ignore_case: ignore_case,
        files_with_matches_only: matches.is_present("files-with-matches"),
        line_number: matches.is_present("line-number") ||
                     matches.is_present("visual-studio-format"),
        with_color: !matches.is_present("nocolor") && !matches.is_present("visual-studio-format") &&
                    is_color_output_available(),
        max_count: matches.value_of("NUM").map(|s| {
            match usize::from_str_radix(s, 10) {
                Ok(n) => n,
                Err(parse_err) => panic!("NUM: {}", parse_err),
            }
        }),
    };

    // Get the index from file
    let index_path = libcsearch::csearch_index();
    let index_reader = match IndexReader::open(index_path) {
        Ok(i) => i,
        Err(e) => panic!("{}", e),
    };

    // Find all possibly matching files using the pseudo-regexp
    let mut post: BTreeSet<u32> = if matches.is_present("bruteforce") {
        index_reader.query(Query::all()).into_inner()
    } else {
        // Get the pseudo-regexp (built using trigrams)
        let expr = regex_syntax::ExprBuilder::new().unicode(false).parse(&pattern).unwrap();
        let q = RegexInfo::new(expr).unwrap().query;
        // panic!("query = {} --- {:?}", q.format_as_string(), q);

        index_reader.query(q).into_inner()
    };
    // println!("identified {} possible queries", post.len());

    // If provided, filter possibly matching files via FILE_PATTERN
    if let Some(ref file_pattern_str) = matches.value_of("FILE_PATTERN") {
        let file_pattern = match Regex::new(&file_pattern_str) {
            Ok(r) => r,
            Err(e) => panic!("FILE_PATTERN: {}", e),
        };
        post = post.into_iter()
            .filter(|file_id| {
                let name = index_reader.name(*file_id);
                file_pattern.is_match(&name)
            })
            .collect::<BTreeSet<_>>();
    }

    // writeln!(io::stderr(), "searching").unwrap();
    let mut buffer = vec![0; 4096];
    let path_simplifier = PathSimplifier::from(&match_options);
    let g: Grep = GrepBuilder::new(&match_options.pattern.as_str()).build().unwrap();
    let matcher = bytes::Regex::new(&match_options.pattern.as_str()).unwrap();
    for file_id in post {
        // println!("next file");
        buffer.clear();
        let name = index_reader.name(file_id);
        // writeln!(io::stderr(), "searching {}", name).unwrap();
        let mut reader = match File::open(&name) {
            Ok(r) => r,
            Err(cause) => {
                warn!("{} - File open failure: {}", name, cause);
                continue;
            }
        };
        let name = path_simplifier.maybe_make_relative(name);
        match reader.read_to_end(&mut buffer) {
            Ok(_) => (),
            Err(_) => continue,
        }
        let mut line_count = 0;
        let mut stdout = if match_options.with_color {
            Stdout::new(ColorChoice::Auto)
        } else {
            Stdout::new(ColorChoice::Never)
        };

        let mut last_line_end = 0;
        if match_options.print_count {
            let count = g.iter(&buffer).count();
            if count != 0 {
                writeln!(&mut stdout, "{}:{}", name.display(), count).unwrap();
            }
            continue;
        }
        for each_match in g.iter(&buffer) {
            if match_options.files_with_matches_only {
                writeln!(&mut stdout, "{}", name.display()).unwrap();
                break;
            }
            stdout.set_color(ColorSpec::new().set_fg(Some(Color::Green))).unwrap();
            write!(&mut stdout, "{}", name.display()).unwrap();
            stdout.set_color(ColorSpec::new().set_fg(None)).unwrap();
            if match_options.print_format == PrintFormat::VisualStudio {
                write!(&mut stdout, "(").unwrap();
            } else {
                write!(&mut stdout, ":").unwrap();
            }
            if match_options.line_number && last_line_end != each_match.end() {
                let num_lines = bytecount::count(&buffer[last_line_end..each_match.start()], b'\n');
                line_count += num_lines + 1;
                last_line_end = each_match.end();
                let line_number = line_count.to_string();
                stdout.set_color(ColorSpec::new().set_fg(Some(Color::Blue))).unwrap();
                stdout.write(&line_number.as_bytes()).unwrap();
                stdout.set_color(ColorSpec::new().set_fg(None)).unwrap();
                if match_options.print_format == PrintFormat::VisualStudio {
                    write!(&mut stdout, ")").unwrap();
                }
                write!(&mut stdout, ":").unwrap();
            }
            let line = &buffer[each_match.start()..each_match.end()];
            if match_options.with_color {
                let mut start_from = 0;
                for m in matcher.find_iter(&line) {
                    let to_write = &line[start_from..m.start()];
                    stdout.write(&to_write).unwrap();
                    stdout.set_color(ColorSpec::new().set_fg(Some(Color::Red))).unwrap();
                    let to_write = &line[m.start()..m.end()];
                    stdout.write(&to_write).unwrap();
                    stdout.set_color(ColorSpec::new().set_fg(None)).unwrap();
                    start_from = m.end();
                }
                if start_from != line.len() {
                    let to_write = &line[start_from..];
                    stdout.write(&to_write).unwrap();
                }
            } else {
                stdout.write(&line).unwrap();
            }
            if line.last() != Some(&b'\n') {
                stdout.write(&[b'\n']).unwrap();
            }
        }
    }

}


struct PathSimplifier {
    make_relative: bool,
}

impl<'a> From<&'a MatchOptions> for PathSimplifier {
    fn from(o: &'a MatchOptions) -> Self {
        PathSimplifier { make_relative: o.print_format != PrintFormat::VisualStudio }
    }
}

impl PathSimplifier {
    fn maybe_make_relative<P: AsRef<Path>>(&self, p: P) -> PathBuf {
        if self.make_relative {
            PathBuf::from(p.as_ref()
                .strip_prefix(&env::current_dir().unwrap())
                .unwrap_or(p.as_ref()))
        } else {
            PathBuf::from(p.as_ref())
        }
    }
}

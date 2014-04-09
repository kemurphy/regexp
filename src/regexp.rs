use collections::HashMap;
use std::from_str::from_str;
use std::str;

use super::Error;
use super::compile::{Inst, compile};
use super::parse::parse;
use super::vm;
use super::vm::CaptureIndices;

/// Regexp is a compiled regular expression.
pub struct Regexp {
    orig: ~str,
    prog: Vec<Inst>,
    names: Vec<Option<~str>>,
}

impl Regexp {
    /// Creates a new compiled regular expression. Once compiled, it can be
    /// used repeatedly to search, split or replace text in a string.
    pub fn new(regex: &str) -> Result<Regexp, Error> {
        let ast = try!(parse(regex));
        let (insts, cap_names) = compile(ast);
        Ok(Regexp {
            orig: regex.to_owned(),
            prog: insts,
            names: cap_names,
        })
    }

    /// Executes the VM on the string given and converts the positions
    /// returned from Unicode character indices to byte indices.
    fn run(&self, text: &str) -> CaptureIndices {
        let search = SearchText::from_str(text, true);
        search.exec(self)
    }

    /// Returns true if and only if the regexp matches the string given.
    pub fn is_match(&self, text: &str) -> bool {
        self.has_match(&SearchText::from_str(text, false).exec(self))
    }

    fn has_match(&self, caps: &CaptureIndices) -> bool {
        caps.len() > 0 && caps.get(0).is_some()
    }

    /// Returns the start and end byte range of the leftmost-longest match in 
    /// `text`. If no match exists, then `None` is returned.
    pub fn find(&self, text: &str) -> Option<(uint, uint)> {
        *self.run(text).get(0)
    }

    /// Iterates through each successive non-overlapping match in `text`,
    /// returning the start and end byte indices with respect to `text`.
    pub fn find_iter<'r>(&'r self, text: &'r str) -> FindMatches<'r> {
        FindMatches {
            re: self,
            search: SearchText::from_str(text, true),
            last_end: 0,
            last_match: 0,
        }
    }

    /// Returns the capture groups corresponding to the leftmost-longest
    /// match in `text`. Capture group `0` always corresponds to the entire 
    /// match. If no match is found, then `None` is returned.
    pub fn captures<'r>(&self, text: &'r str) -> Option<Captures<'r>> {
        let search = SearchText::from_str(text, true);
        let caps = search.exec(self);
        Captures::new(self, &search, caps)
    }

    /// Returns an iterator over all the non-overlapping capture groups matched
    /// in `text`. This is operationally the same as `find_iter` (except it
    /// yields capture groups and not positions).
    pub fn captures_iter<'r>(&'r self, text: &'r str) -> FindCaptures<'r> {
        FindCaptures {
            re: self,
            search: SearchText::from_str(text, true),
            last_match: 0,
            last_end: 0,
        }
    }

    /// Returns an iterator of substrings of `text` delimited by a match
    /// of the regular expression.
    /// Namely, each element of the iterator corresponds to text that *isn't* 
    /// matched by the regular expression.
    pub fn split<'r>(&'r self, text: &'r str) -> RegexpSplits<'r> {
        RegexpSplits {
            finder: self.find_iter(text),
            text: text,
            last: 0,
        }
    }

    /// Returns an iterator of `limit` substrings of `text` delimited by a 
    /// match of the regular expression. (A `limit` of `0` will return no
    /// substrings.)
    /// Namely, each element of the iterator corresponds to text that *isn't* 
    /// matched by the regular expression.
    pub fn splitn<'r>(&'r self, text: &'r str, limit: uint)
                         -> RegexpSplitsN<'r> {
        RegexpSplitsN {
            splits: self.split(text),
            cur: 0,
            limit: limit,
        }
    }

    /// Replaces the leftmost-longest match with the replacement provided.
    /// The replacement can be a regular string (where `$N` and `$name` are
    /// expanded to match capture groups) or a function that takes the matches' 
    /// `Captures` and returns the replaced string.
    ///
    /// If no match is found, then a copy of the string is returned unchanged.
    pub fn replace<R: Replacer>(&self, text: &str, rep: R) -> ~str {
        let caps =
            match self.captures(text) {
                None => return ~"",
                Some(caps) => caps,
            };
        let (s, e) = match caps.pos(0) {
            None => return text.to_owned(), // hmm, switch to MaybeOwned?
            Some((s, e)) => (s, e),
        };
        let mut new = str::with_capacity(text.len());
        new.push_str(text.slice(0, s));
        new.push_str(rep.replace(&caps));
        new.push_str(text.slice(e, text.len()));
        new
    }

    /// Replaces all non-overlapping matches in `text` with the 
    /// replacement provided. This is the same as calling `replacen` with
    /// `limit` set to `0`.
    pub fn replace_all<R: Replacer>(&self, text: &str, rep: R) -> ~str {
        self.replacen(text, 0, rep)
    }

    /// Replaces at most `limit` non-overlapping matches in `text` with the 
    /// replacement provided. If `limit` is 0, then all non-overlapping matches
    /// are replaced.
    pub fn replacen<R: Replacer>
                   (&self, text: &str, limit: uint, rep: R) -> ~str {
        let mut new = str::with_capacity(text.len());
        let mut last_match = 0u;
        let mut i = 0;
        for cap in self.captures_iter(text) {
            // It'd be nicer to use the 'take' iterator instead, but it seemed
            // awkward given that '0' => no limit.
            if limit > 0 && i >= limit {
                break
            }
            i += 1;

            let (s, e) = cap.pos(0).unwrap(); // captures only reports matches
            new.push_str(text.slice(last_match, s));
            new.push_str(rep.replace(&cap));
            last_match = e;
        }
        new.push_str(text.slice(last_match, text.len()));
        new
    }
}

/// NoExpand indicates literal string replacement.
///
/// It can be used with `replace` and `replace_all` to do a literal
/// string replacement without expanding `$name` to their corresponding
/// capture groups.
pub struct NoExpand<'r>(pub &'r str);

/// Expands all instances of `$name` in `text` to the corresponding capture
/// group `name`.
///
/// `name` may be an integer corresponding to the index of the
/// capture group (counted by order of opening parenthesis where `0` is the
/// entire match) or it can be a name (consisting of letters, digits or 
/// underscores) corresponding to a named capture group.
///
/// If `name` isn't a valid capture group (whether the name doesn't exist or
/// isn't a valid index), then it is replaced with the empty string.
///
/// To write a literal `$` use `$$`.
pub fn expand(caps: &Captures, text: &str) -> ~str {
    // How evil can you get?
    // FIXME: Don't use regexes for this. It's completely unnecessary.
    // FIXME: Marginal improvement: get a syntax extension re! to prevent
    //        recompilation every time.
    let re = Regexp::new(r"(^|[^$])\$(\w+)").unwrap();
    re.replace_all(text, |refs: &Captures| -> ~str {
        let (pre, name) = (refs.at(1), refs.at(2));
        pre + match from_str::<uint>(name) {
            None => caps.name(name).to_owned(),
            Some(i) => caps.at(i).to_owned(),
        }
    })
}

/// Replacer describes types that can be used to replace matches in a string.
pub trait Replacer {
    fn replace(&self, caps: &Captures) -> ~str;
}

impl<'r> Replacer for NoExpand<'r> {
    fn replace(&self, _: &Captures) -> ~str {
        let NoExpand(s) = *self;
        s.to_owned()
    }
}

impl<'r> Replacer for &'r str {
    fn replace(&self, caps: &Captures) -> ~str {
        expand(caps, *self)
    }
}

impl<'r> Replacer for 'r |&Captures| -> ~str {
    fn replace(&self, caps: &Captures) -> ~str {
        (*self)(caps)
    }
}

/// Yields all substrings delimited by a regular expression match.
pub struct RegexpSplits<'r> {
    finder: FindMatches<'r>,
    text: &'r str,
    last: uint,
}

impl<'r> Iterator<&'r str> for RegexpSplits<'r> {
    fn next(&mut self) -> Option<&'r str> {
        match self.finder.next() {
            None => {
                if self.last >= self.text.len() {
                    None
                } else {
                    let s = self.text.slice(self.last, self.text.len());
                    self.last = self.text.len();
                    Some(s)
                }
            }
            Some((s, e)) => {
                let text = self.text.slice(self.last, s);
                self.last = e;
                Some(text)
            }
        }
    }
}

/// Yields at most `N` substrings delimited by a regular expression match.
///
/// The last substring will be whatever remains after splitting.
pub struct RegexpSplitsN<'r> {
    splits: RegexpSplits<'r>,
    cur: uint,
    limit: uint,
}

impl<'r> Iterator<&'r str> for RegexpSplitsN<'r> {
    fn next(&mut self) -> Option<&'r str> {
        if self.cur >= self.limit {
            None
        } else {
            self.cur += 1;
            if self.cur >= self.limit {
                Some(self.splits.text.slice(self.splits.last,
                                            self.splits.text.len()))
            } else {
                self.splits.next()
            }
        }
    }
}

/// Captures represents a group of captured strings for a single match.
///
/// The 0th capture always corresponds to the entire match. Each subsequent
/// index corresponds to the next capture group in the regex.
/// If a capture group is named, then the matched string is *also* available
/// via the `name` method. (Note that the 0th capture is always unnamed and so
/// must be accessed with the `at` method.)
pub struct Captures<'r> {
    text: &'r str,
    locs: CaptureIndices,
    named: HashMap<~str, uint>,
    offset: uint,
}

impl<'r> Captures<'r> {
    fn new<'r>(re: &Regexp, search: &SearchText<'r>,
               locs: CaptureIndices) -> Option<Captures<'r>> {
        if !re.has_match(&locs) {
            return None
        }

        let mut named = HashMap::new();
        for (i, name) in re.names.iter().enumerate() {
            match name {
                &None => {},
                &Some(ref name) => {
                    named.insert(name.to_owned(), i);
                }
            }
        }
        Some(Captures {
            text: search.text,
            locs: locs,
            named: named,
            offset: 0,
        })
    }

    /// Returns the start and end positions of the Nth capture group.
    /// Returns `(0, 0)` if `i` is not a valid capture group.
    /// The positions returned are *always* byte indices with respect to the 
    /// original string matched.
    pub fn pos(&self, i: uint) -> Option<(uint, uint)> {
        if i >= self.locs.len() {
            return None
        }
        *self.locs.get(i)
    }

    /// Returns the matched string for the capture group `i`.
    /// If `i` isn't a valid capture group, then the empty string is returned.
    pub fn at(&self, i: uint) -> &'r str {
        match self.pos(i) {
            None => "",
            Some((s, e)) => {
                self.text.slice(s, e)
            }
        }
    }

    /// Returns the matched string for the capture group named `name`.
    /// If `name` isn't a valid capture group, then the empty string is 
    /// returned.
    pub fn name(&self, name: &str) -> &'r str {
        match self.named.find(&name.to_owned()) {
            None => "",
            Some(i) => self.at(*i),
        }
    }

    /// Creates an iterator of all the capture groups in order of appearance
    /// in the regular expression.
    pub fn iter(&'r self) -> SubCaptures<'r> {
        SubCaptures { idx: 0, caps: self, }
    }

    /// Creates an iterator of all the capture group positions in order of 
    /// appearance in the regular expression. Positions are byte indices
    /// in terms of the original string matched.
    pub fn iter_pos(&'r self) -> SubCapturesPos<'r> {
        SubCapturesPos { idx: 0, caps: self, }
    }
}

impl<'r> Container for Captures<'r> {
    fn len(&self) -> uint {
        self.locs.len()
    }
}

/// An iterator over capture groups for a particular match of a regular
/// expression.
pub struct SubCaptures<'r> {
    idx: uint,
    caps: &'r Captures<'r>,
}

impl<'r> Iterator<&'r str> for SubCaptures<'r> {
    fn next(&mut self) -> Option<&'r str> {
        if self.idx < self.caps.len() {
            self.idx += 1;
            Some(self.caps.at(self.idx - 1))
        } else {
            None
        }
    }
}

/// An iterator over capture group positions for a particular match of a 
/// regular expression.
///
/// Positions are byte indices in terms of the original string matched.
pub struct SubCapturesPos<'r> {
    idx: uint,
    caps: &'r Captures<'r>,
}

impl<'r> Iterator<Option<(uint, uint)>> for SubCapturesPos<'r> {
    fn next(&mut self) -> Option<Option<(uint, uint)>> {
        if self.idx < self.caps.len() {
            self.idx += 1;
            Some(self.caps.pos(self.idx - 1))
        } else {
            None
        }
    }
}

/// An iterator that yields all non-overlapping capture groups matching a
/// particular regular expression.
pub struct FindCaptures<'r> {
    re: &'r Regexp,
    search: SearchText<'r>,
    last_match: uint,
    last_end: uint,
}

impl<'r> Iterator<Captures<'r>> for FindCaptures<'r> {
    fn next(&mut self) -> Option<Captures<'r>> {
        if self.last_end > self.search.chars.len() {
            return None
        }

        let uni_caps = self.search.exec_slice(self.re,
                                              self.last_end,
                                              self.search.chars.len());
        let (us, ue) =
            if !self.re.has_match(&uni_caps) {
                return None
            } else {
                uni_caps.get(0).unwrap()
            };
        let char_len = ue - us;

        // Don't accept empty matches immediately following a match.
        // i.e., no infinite loops please.
        if char_len == 0 && self.last_end == self.last_match {
            self.last_end += 1;
            return self.next()
        }

        let bytei = self.search.bytei.as_slice();
        let byte_caps = cap_to_byte_indices(uni_caps, bytei);
        let caps = Captures::new(self.re, &self.search, byte_caps);

        self.last_end = ue;
        self.last_match = self.last_end;
        caps
    }
}

/// An iterator over all non-overlapping matches for a particular string.
///
/// The iterator yields a tuple of integers corresponding to the start and end
/// of the match. The indices are byte offsets.
pub struct FindMatches<'r> {
    re: &'r Regexp,
    search: SearchText<'r>,
    last_match: uint,
    last_end: uint,
}

impl<'r> Iterator<(uint, uint)> for FindMatches<'r> {
    fn next(&mut self) -> Option<(uint, uint)> {
        if self.last_end > self.search.chars.len() {
            return None
        }

        let uni_caps = self.search.exec_slice(self.re,
                                              self.last_end,
                                              self.search.chars.len());
        let (us, ue) =
            if !self.re.has_match(&uni_caps) {
                return None
            } else {
                uni_caps.get(0).unwrap()
            };
        let char_len = ue - us;

        // Don't accept empty matches immediately following a match.
        // i.e., no infinite loops please.
        if char_len == 0 && self.last_end == self.last_match {
            self.last_end += 1;
            return self.next()
        }

        self.last_end = ue;
        self.last_match = self.last_end;
        Some((*self.search.bytei.get(us), *self.search.bytei.get(ue)))
    }
}

struct SearchText<'r> {
    text: &'r str,
    chars: Vec<char>,
    bytei: Vec<uint>,
    caps: bool,
}

// TODO: Choose better names. There's some complicated footwork going on here
// to handle character and byte indices.
impl<'r> SearchText<'r> {
    fn from_str(input: &'r str, caps: bool) -> SearchText<'r> {
        let chars = input.chars().collect();
        let bytei = char_to_byte_indices(input);
        SearchText { text: input, chars: chars, bytei: bytei, caps: caps }
    }

    fn exec(&self, re: &Regexp) -> CaptureIndices {
        let caps = vm::run(re.prog.as_slice(), self.chars.as_slice(), self.caps);
        cap_to_byte_indices(caps, self.bytei.as_slice())
    }

    fn exec_slice(&self, re: &Regexp, us: uint, ue: uint) -> CaptureIndices {
        let chars = self.chars.as_slice().slice(us, ue);
        let caps = vm::run(re.prog.as_slice(), chars, self.caps);
        caps.iter().map(|loc| loc.map(|(s, e)| (us + s, us + e))).collect()
    }
}

impl<'r> Container for SearchText<'r> {
    fn len(&self) -> uint {
        self.text.len()
    }
}

fn cap_to_byte_indices(mut cis: CaptureIndices, bis: &[uint])
                      -> CaptureIndices {
    for v in cis.mut_iter() {
        *v = v.map(|(s, e)| (bis[s], bis[e]))
    }
    cis
}

fn char_to_byte_indices(input: &str) -> Vec<uint> {
    let mut bytei = Vec::with_capacity(input.len());
    for (bi, _) in input.char_indices() {
        bytei.push(bi);
    }
    // Push one more for the length.
    bytei.push(input.len());
    bytei
}

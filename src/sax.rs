// ---------------------------------------------------------------------------
// Sax trait — SAX-style event sink
// ---------------------------------------------------------------------------

/// Receives a stream of structural events as the parser walks the input.
///
/// Implement this trait to produce any output from a single pass over the JSON
/// source.  The built-in implementation used by [`crate::parse_to_tape`]
/// produces a flat [`crate::dom::Tape`].
///
/// A custom implementation can be driven via [`crate::parse_with`] (portable
/// SWAR) or [`crate::parse_with_zmm`] (AVX-512BW assembly).
pub trait Sax<'src> {
    /// The type returned by [`finish`](Sax::finish).
    type Output;

    /// A `null` literal was parsed.
    fn null(&mut self);
    /// A `true` or `false` literal was parsed.
    fn bool_val(&mut self, v: bool);
    /// A JSON number; `s` is a slice of the original source string.
    fn number(&mut self, s: &'src str);
    /// A JSON string value with no escape sequences; `s` borrows from the source.
    fn string(&mut self, s: &'src str);
    /// A JSON string value that contained escape sequences; `s` is the decoded text.
    fn escaped_string(&mut self, s: Box<str>);
    /// An object key with no escape sequences; `s` borrows from the source.
    fn key(&mut self, s: &'src str);
    /// An object key that contained escape sequences; `s` is the decoded text.
    fn escaped_key(&mut self, s: Box<str>);
    /// Opening `{` of an object.
    fn start_object(&mut self);
    /// Closing `}` of an object.
    fn end_object(&mut self);
    /// Opening `[` of an array.
    fn start_array(&mut self);
    /// Closing `]` of an array.
    fn end_array(&mut self);
    /// Called once after the last token; returns the final output or `None` on
    /// internal error.
    fn finish(self) -> Option<Self::Output>;
}

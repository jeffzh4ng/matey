use nom::{
    bytes::complete::{tag, take, take_while1},
    combinator::map_res,
    IResult,
};
use std::collections::BTreeMap;

// TODO: non-UTF8 strings?
// shouldn't happen according to the BitTorrent specification,
// but we need a way to handle that.
pub fn parse_bencode(bencode: &str) -> IResult<&str, Bencode> {
    // TODO: Error propogation
    if let Ok((bencode, output)) = number(bencode) {
        Ok((bencode, Bencode::Number(output)))
    } else if let Ok((bencode, output)) = string(bencode) {
        Ok((bencode, Bencode::String(output)))
    } else {
        // MAX TODO: Return a better error somehow
        Err(nom::Err::Failure(nom::error::Error::new(bencode, nom::error::ErrorKind::Alt)))
    }

    // TODO: implement these parsers
    
    // } else if let Ok((bencode, output)) = list(bencode) {
    //     return Ok((bencode, Bencode::List(output)));
    // } else if let Ok((bencode, output)) = dict(bencode) {
    //     return Ok((bencode, Bencode::Dict(output)));
    // }
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub enum Bencode {
    Number(i64),
    String(String),
    List(Vec<Bencode>),
    Dict(BTreeMap<String, Bencode>),
}

fn string(bencode: &str) -> IResult<&str, String> {
    let (bencode, num_characters) = map_res(take_while1(|c: char| c.is_ascii_digit()), |s: &str| {
        s.parse::<usize>()
    })(bencode)?;
    let (bencode, _) = tag(":")(bencode)?;
    let (bencode, output_string) = take(num_characters)(bencode)?;

    Ok((bencode, output_string.to_owned()))
}

// TODO: leading zeroes?
fn number(bencode: &str) -> IResult<&str, i64> {
    let (bencode, _) = tag("i")(bencode)?;
    let (bencode, output_number) = map_res(take_while1(|c: char| c.is_ascii_digit() || c == '-'), |s: &str| {
        s.parse()
    })(bencode)?;
    let (bencode, _) = tag("e")(bencode)?;
    Ok((bencode, output_number))
}

// fn number(bencode: &str) -> IResult<&str, 

// bencode support
// - numbers: i42e, i-42e, i0e
// - strings: 5:hello
// - lists: l5:hello5:worlde
// - dicts: d5:helloi42ee

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn basic_byte_string() {
        assert_eq!(
            parse_bencode("5:hello"),
            Ok(("", Bencode::String("hello".to_owned())))
        );
    }

    #[test]
    fn smaller_length() {
        assert_eq!(
            parse_bencode("5:helloworld"),
            Ok(("world", Bencode::String("hello".to_owned())))
        )
    }

    #[test]
    fn empty_string() {
        assert_eq!(
            parse_bencode("0:"),
            Ok(("", Bencode::String("".to_owned())))
        )
    }

    #[test]
    fn empty_string_smaller_len() {
        assert_eq!(
            parse_bencode("0:world"),
            Ok(("world", Bencode::String("".to_owned())))
        )
    }

    #[test]
    fn whitespace_in_string() {
        assert_eq!(
            parse_bencode("11:hello world"),
            Ok(("", Bencode::String("hello world".to_owned())))
        )
    }

    #[test]
    fn long_len() {
        assert_eq!(
            parse_bencode("42:helloworldprogrammedtothinkandnottofeeeeel"),
            Ok(("", Bencode::String("helloworldprogrammedtothinkandnottofeeeeel".to_owned())))
        )
    }

    #[test]
    fn long_len_multiple_whitespace_in_string() {
        assert_eq!(
            parse_bencode("50:hello world programmed to think and not to feeeeel"),
            Ok(("", Bencode::String("hello world programmed to think and not to feeeeel".to_owned())))
        )
    }

    #[test]
    fn negative_len_string() {
        assert!(parse_bencode("-2:hello").is_err())
    }

    #[test]
    fn invalid_len_string() {
        assert!(parse_bencode("5a:hello").is_err())
    }

    #[test]
    fn byte_number() {
        assert_eq!(parse_bencode("i88e"), Ok(("", Bencode::Number(88))))
    }
}

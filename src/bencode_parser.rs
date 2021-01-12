use nom::{
    bytes::complete::{tag, take, take_while1},
    combinator::map_res,
    multi::many0,
    IResult,
};
use std::collections::BTreeMap;

// TODO: non-UTF8 strings?
// shouldn't happen according to the BitTorrent specification,
// but we need a way to handle that.
pub fn parse_bencode(bencode: &str) -> IResult<&str, Bencode> {
    dbg!(bencode);

    // TODO: Error propogation
    if let Ok((bencode, output)) = number(bencode) {
        Ok((bencode, Bencode::Number(output)))
    } else if let Ok((bencode, output)) = string(bencode) {
        Ok((bencode, Bencode::String(output)))
    } else if let Ok((bencode, output)) = list(bencode) {
        Ok((bencode, Bencode::List(output)))
    } else {
        // MAX TODO: Return a better error somehow
        Err(nom::Err::Error(nom::error::Error::new(
            bencode,
            nom::error::ErrorKind::Alt,
        )))
    }

    // else if let Ok((bencode, output)) = dict(bencode) {
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
    let (bencode, num_characters) =
        map_res(take_while1(|c: char| c.is_ascii_digit()), |s: &str| {
            s.parse::<usize>()
        })(bencode)?;
    let (bencode, _) = tag(":")(bencode)?;
    let (bencode, output_string) = take(num_characters)(bencode)?;

    Ok((bencode, output_string.to_owned()))
}

// TODO: leading zeroes?
// TODO: add comprehensive tests
fn number(bencode: &str) -> IResult<&str, i64> {
    let (bencode, _) = tag("i")(bencode)?;
    let (bencode, output_number) = map_res(
        take_while1(|c: char| c.is_ascii_digit() || c == '-'),
        |s: &str| s.parse(),
    )(bencode)?;
    let (bencode, _) = tag("e")(bencode)?;
    Ok((bencode, output_number))
}

fn list(bencode: &str) -> IResult<&str, Vec<Bencode>> {
    let (bencode, _) = tag("l")(bencode)?;
    let (bencode, output_list) = many0(parse_bencode)(bencode)?;
    let (bencode, _) = tag("e")(bencode)?;

    Ok((bencode, output_list))
}

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
        );
    }

    #[test]
    fn empty_string() {
        assert_eq!(
            parse_bencode("0:"),
            Ok(("", Bencode::String("".to_owned())))
        );
    }

    #[test]
    fn empty_string_smaller_len() {
        assert_eq!(
            parse_bencode("0:world"),
            Ok(("world", Bencode::String("".to_owned())))
        );
    }

    #[test]
    fn whitespace_in_string() {
        assert_eq!(
            parse_bencode("11:hello world"),
            Ok(("", Bencode::String("hello world".to_owned())))
        );
    }

    #[test]
    fn long_len() {
        assert_eq!(
            parse_bencode("42:helloworldprogrammedtothinkandnottofeeeeel"),
            Ok((
                "",
                Bencode::String("helloworldprogrammedtothinkandnottofeeeeel".to_owned())
            ))
        );
    }

    #[test]
    fn long_len_multiple_whitespace_in_string() {
        assert_eq!(
            parse_bencode("50:hello world programmed to think and not to feeeeel"),
            Ok((
                "",
                Bencode::String("hello world programmed to think and not to feeeeel".to_owned())
            ))
        );
    }

    #[test]
    fn negative_len_string() {
        assert!(parse_bencode("-2:hello").is_err());
    }

    #[test]
    fn incorrect_len_string() {
        assert!(parse_bencode("5:worl").is_err());
    }

    #[test]
    fn invalid_len_string() {
        assert!(parse_bencode("5a:hello").is_err());
    }

    #[test]
    fn basic_number() {
        assert_eq!(parse_bencode("i88e"), Ok(("", Bencode::Number(88))));
    }

    #[test]
    fn basic_list() {
        let bencode_hello = "5:hello";
        let bencode_world = "5:world";

        assert_eq!(
            parse_bencode(&format!("l{}{}e", bencode_hello, bencode_world)),
            Ok((
                "",
                Bencode::List(vec![
                    Bencode::String("hello".to_owned()),
                    Bencode::String("world".to_owned())
                ])
            ))
        );
    }

    #[test]
    fn heterogenous_list() {
        let bencode_string = "5:hello";
        let bencode_number = "i8e";

        assert_eq!(
            parse_bencode(&format!("l{}{}e", bencode_string, bencode_number)),
            Ok((
                "",
                Bencode::List(vec![
                    Bencode::String("hello".to_owned()),
                    Bencode::Number(8)
                ])
            ))
        );
    }

    #[test]
    fn multiple_nested_list() {
        let list_one = "l5:hello5:worlde";
        let list_two = "l5:helloi8ee";

        assert_eq!(
            parse_bencode(&format!("l{}{}e", list_one, list_two)),
            Ok((
                "",
                Bencode::List(vec![
                    Bencode::List(vec![
                        Bencode::String("hello".to_owned()),
                        Bencode::String("world".to_owned())
                    ]),
                    Bencode::List(vec![
                        Bencode::String("hello".to_owned()),
                        Bencode::Number(8)
                    ])
                ])
            ))
        );
    }

    #[test]
    fn empty_list() {
        assert_eq!(parse_bencode("le"), Ok(("", Bencode::List(vec![]))));
    }

    #[test]
    fn incomplete_list() {
        let bencode_number = "i8e";
        assert!(parse_bencode(&format!("l{0}{0}{0}", bencode_number)).is_err());
    }

    #[test]
    fn list_with_invalid_element() {
        let invalid_bencode_string = "-5:hello";

        assert!(parse_bencode(&format!("l{}e", invalid_bencode_string)).is_err());
    } 
}

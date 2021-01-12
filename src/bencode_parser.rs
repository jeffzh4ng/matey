use nom::{
    bytes::complete::{tag, take, take_while1},
    combinator::map_res,
    multi::many0,
    sequence::pair,
    IResult,
};
use snafu::Snafu;
use std::{collections::BTreeMap, num};

// TODO: non-UTF8 strings?
// shouldn't happen according to the BitTorrent specification,
// but we need a way to handle that.
pub fn parse_bencode(bencode: &str) -> IResult<&str, Bencode> {
    // TODO: Error propogation
    if let Ok((bencode, output)) = number(bencode) {
        Ok((bencode, Bencode::Number(output)))
    } else if let Ok((bencode, output)) = string(bencode) {
        Ok((bencode, Bencode::String(output)))
    } else if let Ok((bencode, output)) = list(bencode) {
        Ok((bencode, Bencode::List(output)))
    } else if let Ok((bencode, output)) = dict(bencode) {
        return Ok((bencode, Bencode::Dict(output)));
    } else {
        // TODO: Return a better error somehow
        Err(nom::Err::Error(nom::error::Error::new(
            bencode,
            nom::error::ErrorKind::Alt,
        )))
    }
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub enum Bencode {
    Number(i64),
    String(String),
    List(Vec<Bencode>),
    Dict(BTreeMap<String, Bencode>),
}

#[non_exhaustive]
#[derive(Debug, Snafu)]
pub enum BencodeParsingError {
    #[snafu(display("Invalid number: {}", source))]
    InvalidNumber { source: BencodeNumberParsingError },
    #[snafu(display("Input is not valid bencode"))]
    InvalidBencode,
}

#[non_exhaustive]
#[derive(Debug, Snafu)]
pub enum BencodeNumberParsingError {
    #[snafu(display("Zero cannot be negative"))]
    NegativeZero,
    #[snafu(display("Numbers cannot have leading zeroes"))]
    LeadingZero,
    #[snafu(display("Numbers cannot be empty"))]
    EmptyNumber,
    ParseError {
        source: num::ParseIntError,
    },
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

fn number(bencode: &str) -> IResult<&str, i64> {
    let (bencode, _) = tag("i")(bencode)?;
    let (bencode, output_number) = map_res(
        take_while1(|c: char| c.is_ascii_digit() || c == '-'),
        |s: &str| {
            let number = s
                .parse()
                .map_err(|e| BencodeNumberParsingError::ParseError { source: e })?;
            let first_char = s
                .chars()
                .next()
                .ok_or(BencodeNumberParsingError::EmptyNumber)?;

            if first_char == '-' {
                // There must be a second character, otherwise parse would have failed
                if s.chars().nth(1).unwrap() == '0' {
                    return if number == 0 {
                        Err(BencodeNumberParsingError::NegativeZero)
                    } else {
                        Err(BencodeNumberParsingError::LeadingZero)
                    };
                }
            } else if first_char == '0' && s.len() > 1 {
                return Err(BencodeNumberParsingError::LeadingZero);
            }

            Ok(number)
        },
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

fn dict(bencode: &str) -> IResult<&str, BTreeMap<String, Bencode>> {
    let (bencode, _) = tag("d")(bencode)?;
    let (bencode, output_tuple_list) = many0(pair(string, parse_bencode))(bencode)?;
    let (bencode, _) = tag("e")(bencode)?;

    Ok((bencode, output_tuple_list.into_iter().collect()))
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
    fn positive_number() {
        assert_eq!(parse_bencode("i88e"), Ok(("", Bencode::Number(88))));
    }

    #[test]
    fn zero() {
        assert_eq!(parse_bencode("i0e"), Ok(("", Bencode::Number(0))));
    }

    #[test]
    fn negative_number() {
        assert_eq!(parse_bencode("i-88e"), Ok(("", Bencode::Number(-88))));
    }

    #[test]
    fn empty_number() {
        assert!(parse_bencode("ie").is_err());
    }

    #[test]
    fn negative_zero() {
        assert!(parse_bencode("i-0e").is_err());
    }

    #[test]
    fn positive_leading_zero() {
        assert!(parse_bencode("i08e").is_err());
    }

    #[test]
    fn negative_leading_zero() {
        assert!(parse_bencode("i-08e").is_err());
    }

    #[test]
    fn positive_multiple_zeroes() {
        assert!(parse_bencode("i000e").is_err());
    }

    #[test]
    fn negative_multiple_zeroes() {
        assert!(parse_bencode("i-000e").is_err());
    }

    #[test]
    fn only_negative_sign() {
        assert!(parse_bencode("i-e").is_err());
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

    #[test]
    fn basic_dict() {
        let key_one = "3:bar";
        let val_one = "4:spam";

        let key_two = "3:foo";
        let val_two = "i88e";

        assert_eq!(
            parse_bencode(&format!("d{}{}{}{}e", key_one, val_one, key_two, val_two)),
            Ok((
                "",
                Bencode::Dict(
                    vec![
                        ("bar".to_owned(), Bencode::String("spam".to_owned())),
                        ("foo".to_owned(), Bencode::Number(88)),
                    ]
                    .into_iter()
                    .collect()
                )
            ))
        );
    }

    #[test]
    fn empty_dict() {
        assert_eq!(
            parse_bencode("de"),
            Ok(("", Bencode::Dict(BTreeMap::new())))
        );
    }

    #[test]
    fn incomplete_dict() {
        let key_one = "3:foo";

        assert!(parse_bencode(&format!("d{}e", key_one)).is_err());
    }

    #[test]
    fn dict_with_invalid_key() {
        let key_one = "-3:foo";
        let val_one = "i88e";

        assert!(parse_bencode(&format!("d{}{}e", key_one, val_one)).is_err());
    }

    #[test]
    fn dict_with_invalid_value() {
        let key_one = "3:foo";
        let val_one = "-3:bar";

        assert!(parse_bencode(&format!("d{}{}e", key_one, val_one)).is_err());
    }

    // Integration tests
    // TODO: check if rust has clean way for test separation

    #[test]
    fn list_of_dict() {
        let key_one = "3:foo";
        let val_one = "3:bar";

        let key_two = "3:baz";
        let val_two = "3:baz";

        let dict_str = &format!("d{}{}{}{}e", key_one, val_one, key_two, val_two);
        let (_, result_dict) = dict(dict_str).unwrap();

        assert_eq!(
            parse_bencode(&format!("l{0}{0}e", dict_str)),
            Ok((
                "",
                Bencode::List(vec![
                    Bencode::Dict(result_dict.clone()),
                    Bencode::Dict(result_dict)
                ])
            ))
        );
    }

    #[test]
    fn dict_of_list() {
        let bencode_hello = "5:hello";
        let bencode_world = "5:world";

        let list_str = &format!("l{}{}e", bencode_hello, bencode_world);

        let key_one = "3:foo";
        let key_two = "3:bar";

        let (_, result_list) = list(list_str).unwrap();

        assert_eq!(
            parse_bencode(&format!("d{}{2}{}{2}e", key_one, key_two, list_str)),
            Ok((
                "",
                Bencode::Dict(
                    vec![
                        ("foo".to_owned(), Bencode::List(result_list.clone())),
                        ("bar".to_owned(), Bencode::List(result_list)),
                    ]
                    .into_iter()
                    .collect()
                )
            ))
        );
    }

    #[test]
    fn multiple_nested_dicts() {
        let key_one = "3:foo";
        let val_one = "3:bar";

        let key_two = "3:baz";
        let val_two = "3:baz";

        let nested_dict_str = &format!("d{}{}{}{}e", key_one, val_one, key_two, val_two);
        let (_, result_nested_dict) = dict(nested_dict_str).unwrap();

        assert_eq!(
            parse_bencode(&format!("d{}{2}{}{2}e", key_one, key_two, nested_dict_str)),
            Ok((
                "",
                Bencode::Dict(
                    vec![
                        ("foo".to_owned(), Bencode::Dict(result_nested_dict.clone())),
                        ("baz".to_owned(), Bencode::Dict(result_nested_dict)),
                    ]
                    .into_iter()
                    .collect()
                )
            ))
        );
    }
}

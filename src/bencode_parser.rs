use nom::{
    branch::alt,
    bytes::complete::{tag, take, take_while1},
    character::is_digit,
    combinator::{cut, map, map_res},
    multi::many0,
    sequence::pair,
    IResult,
};
use snafu::Snafu;
use std::{collections::BTreeMap, num};

pub fn parse_bencode(bencode: &[u8]) -> IResult<&[u8], Bencode> {
    // TODO: return our custom error somehow
    alt((
        map(number, Bencode::Number),
        map(string, Bencode::ByteString),
        map(list, Bencode::List),
        map(dict, Bencode::Dict),
    ))(bencode)
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub enum Bencode {
    Number(i64),
    ByteString(Vec<u8>),
    List(Vec<Bencode>),
    Dict(BTreeMap<Vec<u8>, Bencode>),
}

impl Bencode {
    pub fn number(self) -> Option<i64> {
        if let Self::Number(num) = self {
            Some(num)
        } else {
            None
        }
    }

    pub fn byte_string(self) -> Option<Vec<u8>> {
        if let Self::ByteString(bytes) = self {
            Some(bytes)
        } else {
            None
        }
    }

    pub fn list(self) -> Option<Vec<Bencode>> {
        if let Self::List(list) = self {
            Some(list)
        } else {
            None
        }
    }

    pub fn dict(self) -> Option<BTreeMap<Vec<u8>, Bencode>> {
        if let Self::Dict(dict) = self {
            Some(dict)
        } else {
            None
        }
    }
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

fn string(bencode: &[u8]) -> IResult<&[u8], Vec<u8>> {
    let (bencode, num_characters) = map_res(take_while1(is_digit), |bytes| {
        String::from_utf8_lossy(bytes).parse::<usize>()
    })(bencode)?;
    let (bencode, _) = cut(tag(":"))(bencode)?;
    let (bencode, output_string) = cut(take(num_characters))(bencode)?;

    Ok((bencode, output_string.to_vec()))
}

fn number(bencode: &[u8]) -> IResult<&[u8], i64> {
    let (bencode, _) = tag("i")(bencode)?;
    let (bencode, output_number) = cut(map_res(
        take_while1(|c| is_digit(c) || c == b'-'),
        |bytes| {
            let s = String::from_utf8_lossy(bytes);

            // TODO: clean up this error handling
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
    ))(bencode)?;
    let (bencode, _) = cut(tag("e"))(bencode)?;

    Ok((bencode, output_number))
}

fn list(bencode: &[u8]) -> IResult<&[u8], Vec<Bencode>> {
    let (bencode, _) = tag("l")(bencode)?;
    let (bencode, output_list) = cut(many0(parse_bencode))(bencode)?;
    let (bencode, _) = cut(tag("e"))(bencode)?;

    Ok((bencode, output_list))
}

fn dict(bencode: &[u8]) -> IResult<&[u8], BTreeMap<Vec<u8>, Bencode>> {
    let (bencode, _) = tag("d")(bencode)?;
    let (bencode, output_tuple_list) = cut(many0(pair(string, parse_bencode)))(bencode)?;
    let (bencode, _) = cut(tag("e"))(bencode)?;

    Ok((bencode, output_tuple_list.into_iter().collect()))
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn basic_byte_string() {
        assert_eq!(
            parse_bencode(b"5:hello"),
            Ok((b"" as &[u8], Bencode::ByteString(b"hello".to_vec())))
        );
    }

    #[test]
    fn smaller_length() {
        assert_eq!(
            parse_bencode(b"5:helloworld"),
            Ok((b"world" as &[u8], Bencode::ByteString(b"hello".to_vec())))
        );
    }

    #[test]
    fn empty_string() {
        assert_eq!(
            parse_bencode(b"0:"),
            Ok((b"" as &[u8], Bencode::ByteString(b"".to_vec())))
        );
    }

    #[test]
    fn empty_string_smaller_len() {
        assert_eq!(
            parse_bencode(b"0:world"),
            Ok((b"world" as &[u8], Bencode::ByteString(b"".to_vec())))
        );
    }

    #[test]
    fn whitespace_in_string() {
        assert_eq!(
            parse_bencode(b"11:hello world"),
            Ok((b"" as &[u8], Bencode::ByteString(b"hello world".to_vec())))
        );
    }

    #[test]
    fn long_len() {
        assert_eq!(
            parse_bencode(b"42:helloworldprogrammedtothinkandnottofeeeeel"),
            Ok((
                b"" as &[u8],
                Bencode::ByteString(b"helloworldprogrammedtothinkandnottofeeeeel".to_vec())
            ))
        );
    }

    #[test]
    fn long_len_multiple_whitespace_in_string() {
        assert_eq!(
            parse_bencode(b"50:hello world programmed to think and not to feeeeel"),
            Ok((
                b"" as &[u8],
                Bencode::ByteString(b"hello world programmed to think and not to feeeeel".to_vec())
            ))
        );
    }

    #[test]
    fn negative_len_string() {
        assert!(parse_bencode(b"-2:hello").is_err());
    }

    #[test]
    fn incorrect_len_string() {
        assert!(parse_bencode(b"5:worl").is_err());
    }

    #[test]
    fn invalid_len_string() {
        assert!(parse_bencode(b"5a:hello").is_err());
    }

    #[test]
    fn positive_number() {
        assert_eq!(
            parse_bencode(b"i88e"),
            Ok((b"" as &[u8], Bencode::Number(88)))
        );
    }

    #[test]
    fn zero() {
        assert_eq!(
            parse_bencode(b"i0e"),
            Ok((b"" as &[u8], Bencode::Number(0)))
        );
    }

    #[test]
    fn negative_number() {
        assert_eq!(
            parse_bencode(b"i-88e"),
            Ok((b"" as &[u8], Bencode::Number(-88)))
        );
    }

    #[test]
    fn empty_number() {
        assert!(parse_bencode(b"ie").is_err());
    }

    #[test]
    fn negative_zero() {
        assert!(parse_bencode(b"i-0e").is_err());
    }

    #[test]
    fn positive_leading_zero() {
        assert!(parse_bencode(b"i08e").is_err());
    }

    #[test]
    fn negative_leading_zero() {
        assert!(parse_bencode(b"i-08e").is_err());
    }

    #[test]
    fn positive_multiple_zeroes() {
        assert!(parse_bencode(b"i000e").is_err());
    }

    #[test]
    fn negative_multiple_zeroes() {
        assert!(parse_bencode(b"i-000e").is_err());
    }

    #[test]
    fn only_negative_sign() {
        assert!(parse_bencode(b"i-e").is_err());
    }

    #[test]
    fn basic_list() {
        let bencode_hello = "5:hello";
        let bencode_world = "5:world";

        assert_eq!(
            parse_bencode(format!("l{}{}e", bencode_hello, bencode_world).as_bytes()),
            Ok((
                b"" as &[u8],
                Bencode::List(vec![
                    Bencode::ByteString(b"hello".to_vec()),
                    Bencode::ByteString(b"world".to_vec())
                ])
            ))
        );
    }

    #[test]
    fn heterogenous_list() {
        let bencode_string = "5:hello";
        let bencode_number = "i8e";

        assert_eq!(
            parse_bencode(format!("l{}{}e", bencode_string, bencode_number).as_bytes()),
            Ok((
                b"" as &[u8],
                Bencode::List(vec![
                    Bencode::ByteString(b"hello".to_vec()),
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
            parse_bencode(format!("l{}{}e", list_one, list_two).as_bytes()),
            Ok((
                b"" as &[u8],
                Bencode::List(vec![
                    Bencode::List(vec![
                        Bencode::ByteString(b"hello".to_vec()),
                        Bencode::ByteString(b"world".to_vec())
                    ]),
                    Bencode::List(vec![
                        Bencode::ByteString(b"hello".to_vec()),
                        Bencode::Number(8)
                    ])
                ])
            ))
        );
    }

    #[test]
    fn empty_list() {
        assert_eq!(
            parse_bencode(b"le"),
            Ok((b"" as &[u8], Bencode::List(vec![])))
        );
    }

    #[test]
    fn incomplete_list() {
        let bencode_number = "i8e";
        assert!(parse_bencode(format!("l{0}{0}{0}", bencode_number).as_bytes()).is_err());
    }

    #[test]
    fn list_with_invalid_element() {
        let invalid_bencode_string = "-5:hello";

        assert!(parse_bencode(format!("l{}e", invalid_bencode_string).as_bytes()).is_err());
    }

    #[test]
    fn basic_dict() {
        let key_one = "3:bar";
        let val_one = "4:spam";

        let key_two = "3:foo";
        let val_two = "i88e";

        assert_eq!(
            parse_bencode(format!("d{}{}{}{}e", key_one, val_one, key_two, val_two).as_bytes()),
            Ok((
                b"" as &[u8],
                Bencode::Dict(
                    vec![
                        (b"bar".to_vec(), Bencode::ByteString(b"spam".to_vec())),
                        (b"foo".to_vec(), Bencode::Number(88)),
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
            parse_bencode(b"de"),
            Ok((b"" as &[u8], Bencode::Dict(BTreeMap::new())))
        );
    }

    #[test]
    fn incomplete_dict() {
        let key_one = "3:foo";

        assert!(parse_bencode(format!("d{}e", key_one).as_bytes()).is_err());
    }

    #[test]
    fn dict_with_invalid_key() {
        let key_one = "-3:foo";
        let val_one = "i88e";

        assert!(parse_bencode(format!("d{}{}e", key_one, val_one).as_bytes()).is_err());
    }

    #[test]
    fn dict_with_invalid_value() {
        let key_one = "3:foo";
        let val_one = "-3:bar";

        assert!(parse_bencode(format!("d{}{}e", key_one, val_one).as_bytes()).is_err());
    }

    // Integration tests
    // TODO: check if rust has clean way for test separation

    #[test]
    fn list_of_dict() {
        let key_one = "3:foo";
        let val_one = "3:bar";

        let key_two = "3:baz";
        let val_two = "3:baz";

        let dict_str = format!("d{}{}{}{}e", key_one, val_one, key_two, val_two);
        let (_, result_dict) = dict(&dict_str.as_bytes()).unwrap();

        assert_eq!(
            parse_bencode(format!("l{0}{0}e", dict_str).as_bytes()),
            Ok((
                b"" as &[u8],
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

        let list_str = format!("l{}{}e", bencode_hello, bencode_world);

        let key_one = "3:foo";
        let key_two = "3:bar";

        let (_, result_list) = list(&list_str.as_bytes()).unwrap();

        assert_eq!(
            parse_bencode(format!("d{}{2}{}{2}e", key_one, key_two, list_str).as_bytes()),
            Ok((
                b"" as &[u8],
                Bencode::Dict(
                    vec![
                        (b"foo".to_vec(), Bencode::List(result_list.clone())),
                        (b"bar".to_vec(), Bencode::List(result_list)),
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

        let nested_dict_str = format!("d{}{}{}{}e", key_one, val_one, key_two, val_two);
        let (_, result_nested_dict) = dict(&nested_dict_str.as_bytes()).unwrap();

        assert_eq!(
            parse_bencode(format!("d{}{2}{}{2}e", key_one, key_two, nested_dict_str).as_bytes()),
            Ok((
                b"" as &[u8],
                Bencode::Dict(
                    vec![
                        (b"foo".to_vec(), Bencode::Dict(result_nested_dict.clone())),
                        (b"baz".to_vec(), Bencode::Dict(result_nested_dict)),
                    ]
                    .into_iter()
                    .collect()
                )
            ))
        );
    }
}

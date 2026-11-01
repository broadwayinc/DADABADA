//! `ddbd debug`: shows encodings, for checking by hand.

use crate::codec::ids;
use crate::codec::keys::{dec_num, enc_num, sort_idx, typed, Val, S_IDX};
use crate::codec::loose::loosen;
use crate::codec::text::{rev_chars, rev_name};

fn parse_val(s: &str) -> Val {
    if let Ok(n) = s.parse::<f64>() {
        if n.is_finite() {
            return Val::Num(n);
        }
    }
    match s {
        "true" => Val::Bool(true),
        "false" => Val::Bool(false),
        _ => Val::Str(s.to_string()),
    }
}

fn show(b: &[u8]) -> String {
    b.iter()
        .map(|c| if c.is_ascii_graphic() { format!("{}", *c as char) } else { format!("\\x{c:02x}") })
        .collect()
}

pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "Usage:\n  ddbd debug key <name> <value>\n  ddbd debug sort <value> <value> ...\n  ddbd debug loose <text>\n  ddbd debug reverse <name or text>\n  ddbd debug id <user_id>";
    let Some(cmd) = args.first() else { return Err(usage.into()) };
    let rest = &args[1..];
    match cmd.as_str() {
        "key" if rest.len() >= 2 => {
            let v = parse_val(&rest[1..].join(" "));
            let k = sort_idx(S_IDX, &rest[0], &typed(&v));
            println!("value  {v:?}\nsort   {}\nhex    {}", show(&k), hex::encode(&k));
        }
        "sort" if !rest.is_empty() => {
            let mut vs: Vec<(Vec<u8>, String)> = rest.iter().map(|s| (typed(&parse_val(s)), s.clone())).collect();
            vs.sort();
            println!("byte order:");
            for (b, s) in vs {
                println!("  {:<24} {}", s, hex::encode(&b));
            }
        }
        "num" if !rest.is_empty() => {
            for s in rest {
                let n: f64 = s.parse().map_err(|_| format!("{s} is not a number"))?;
                let e = enc_num(n);
                println!("{s:<20} {}  (decodes to {})", hex::encode(e), dec_num(&e));
            }
        }
        "loose" if !rest.is_empty() => {
            let t = rest.join(" ");
            let l = loosen(&t);
            println!("text      {t}\nloose     {}\nreversed  {}", l.text, rev_chars(&l.text));
            if l.has_syllable {
                println!("initials  {}", l.initials);
            }
            if l.is_initials_query() {
                println!("as a query: searches the Korean initials key");
            }
        }
        "reverse" if !rest.is_empty() => {
            let t = rest.join(" ");
            println!("name   {}\ntext   {}", rev_name(&t), rev_chars(&t));
        }
        "id" if !rest.is_empty() => {
            let id = ids::new_record_id(&rest[0], ids::now_ms());
            println!("record_id {id}\nuploaded  {}", ids::record_uploaded(&id));
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}

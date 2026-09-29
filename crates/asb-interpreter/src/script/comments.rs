//! Remove scenario block comments before directives can see their contents.
use std::borrow::Cow;
use crate::error::{Error, Result};

pub(super) fn strip_blocks(content: &str) -> Result<Cow<'_, str>> {
    if !content.contains("/*") { return Ok(Cow::Borrowed(content)); }
    let mut result=String::with_capacity(content.len());
    let mut block_start=None;
    let mut in_lua=false;
    for (index, raw) in content.split_inclusive('\n').enumerate() {
        if in_lua {
            result.push_str(raw);
            if raw.trim()=="[/lua]" { in_lua=false; }
            continue;
        }
        let (line,newline)=raw.strip_suffix('\n').map_or((raw,""),|s|(s,"\n"));
        let mut out=String::with_capacity(line.len());
        let mut offset=0;
        let mut tag_start=None;
        let mut quote=None;
        let mut text_start=0;
        let mut removed=block_start.is_some();
        while offset<line.len() {
            let rest=&line[offset..];
            if block_start.is_some() {
                removed=true;
                if let Some(end)=rest.find("*/") {
                    offset+=end+2;block_start=None;out.push(' ');
                    continue;
                }
                break;
            }
            // Match the existing parser's full-line and trailing // comments.
            if tag_start.is_none() && ((out.trim().is_empty() && rest.starts_with(';'))
                || (out[text_start..].trim().is_empty() && rest.starts_with("//"))) {
                out.push_str(rest);break;
            }
            // Tag values, including glob paths and quoted expressions, are data.
            if tag_start.is_none() && rest.starts_with("/*") {
                block_start=Some(index+1);offset+=2;removed=true;out.push(' ');continue;
            }
            let c=rest.chars().next().unwrap();offset+=c.len_utf8();
            if let Some(start)=tag_start {
                if let Some(q)=quote {
                    if c==q { quote=None; }
                } else if c=='"' { quote=Some(c); }
                else if c==']' {
                    let lua=out[start..].trim()=="lua";
                    out.push(c);tag_start=None;text_start=out.len();
                    if lua { in_lua=true;out.push_str(&line[offset..]);break; }
                    continue;
                }
            } else if c=='[' { tag_start=Some(out.len()+1); }
            out.push(c);
        }
        // A removed comment is not a blank line: do not trigger blankline hooks.
        if removed && out.trim().is_empty() { result.push_str("//"); }
        else { result.push_str(&out); }
        result.push_str(newline);
    }
    if let Some(line)=block_start {
        return Err(Error::ParseError{line,message:"未找到 */ 块注释结束标记".into()});
    }
    Ok(Cow::Owned(result))
}

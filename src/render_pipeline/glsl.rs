//! Fixed pixel-effect wrapper used by mobile and console GLSL assets.
//! Keep formulas and uniforms intact; reject unsupported inputs instead of
//! selecting a Windows effect by filename.
use super::hlsl::{matching_brace,replace_identifier,strip_comments};

pub fn to_hlsl(data:&[u8])->Result<String,String>{
    // Legacy packages use non-UTF-8 comments. Discard comments before checking
    // decoding errors; invalid bytes in executable source still fail closed.
    let decoded=String::from_utf8_lossy(data);
    let clean=strip_comments(decoded.trim_start_matches('\u{feff}'));
    if clean.contains('\u{fffd}'){return Err("invalid shader source encoding outside comments".into());}
    let mut source=String::new();
    for line in clean.lines(){
        let line=line.trim();
        if line.starts_with('#') {
            if matches!(line,"#version 100"|"#version 120"|"#version 330"|"#version 330 core"|"#version 300 es"){continue;}
            return Err("unsupported GLSL preprocessor directive".into());
        }
        source.push_str(line);source.push('\n');
    }
    let at=source.find("void main").ok_or("missing fixed GLSL main wrapper")?;
    let open=at+source[at..].find('{').ok_or("missing main body")?;
    let signature:String=source[at..open].chars().filter(|c|!c.is_whitespace()).collect();
    if signature!="voidmain()"&&signature!="voidmain(void)"{return Err("unsupported GLSL entry point".into());}
    let close=matching_brace(&source,open).ok_or("unbalanced GLSL main")?;
    if !source[close+1..].trim().is_empty(){return Err("extra GLSL functions are unsupported".into());}
    let mut prefix=source[..at].to_string();
    // Some legacy sources contain an unused helper (occasionally with invalid
    // uniform qualifiers). A definition has no effect until called. Never drop
    // a helper referenced by main or by another declaration.
    while let Some(open)=prefix.find('{') {
        let close=matching_brace(&prefix,open).ok_or("unbalanced GLSL helper")?;
        let start=prefix[..open].rfind(';').map_or(0,|p|p+1);
        let signature=prefix[start..open].trim();
        let left=signature.find('(').ok_or("unsupported GLSL global block")?;
        let name=signature[..left].split_whitespace().last().ok_or("helper name")?.to_string();
        let remainder=format!("{}{}{}",&prefix[..start],&prefix[close+1..],&source[at..]);
        if name.is_empty()||remainder.split(|c:char|!c.is_ascii_alphanumeric()&&c!='_').any(|t|t==name){
            return Err("called GLSL helpers require a separate port".into());
        }
        prefix.replace_range(start..=close,"");
    }
    let mut globals=String::new();
    for item in prefix.split(';'){
        let item=item.trim();if item.is_empty(){continue;}
        let words:Vec<_>=item.split_whitespace().collect();
        if words.first()==Some(&"precision") {
            if words.len()!=3||words[2]!="float"||!matches!(words[1],"lowp"|"mediump"|"highp"){
                return Err("unsupported GLSL precision declaration".into());
            }continue;
        }
        if matches!(words.first(),Some(&"varying")|Some(&"in")) {
            if words.len()!=3||words[1]!="vec2"||!matches!(words[2],"resultCoord0"|"resultCoord1"){
                return Err("custom GLSL varying requires a separate port".into());
            }continue;
        }
        let item=item.strip_prefix("uniform ").unwrap_or(item).trim();
        if let Some(name)=item.strip_prefix("sampler2D ") {
            if !matches!(name.trim(),"textureBack"|"textureFore"|"textureMask"|"textureUser"){
                return Err("unsupported GLSL sampler".into());
            }continue;
        }
        if !item.starts_with("const ") && words.first()!=Some(&"uniform") {
            return Err("unsupported GLSL global declaration".into());
        }
        globals.push_str(item);globals.push_str(";\n");
    }
    let body=&source[open+1..close];
    let tokens:Vec<_>=body.split(|c:char|!c.is_ascii_alphanumeric()&&c!='_').collect();
    if tokens.iter().any(|t|matches!(*t,"textureBack"|"gl_FragCoord"|"gl_FragData"|"gl_FragDepth")){
        return Err("GLSL destination/depth coordinates require a separate port".into());
    }
    if !tokens.contains(&"gl_FragColor"){return Err("missing GLSL fragment output".into());}
    let convert=|text:&str|{
        let mut s=text.to_string();
        for (from,to) in [("vec2","float2"),("vec3","float3"),("vec4","float4"),
            ("texture2D","tex2D"),("texture","tex2D"),("textureFore","samplerFore"),
            ("textureMask","samplerMask"),("textureUser","samplerUser"),
            ("resultCoord0","texCoord0"),("resultCoord1","texCoord1"),("gl_FragColor","result"),
            ("mix","lerp"),("fract","frac")] {s=replace_identifier(&s,from,to);}
        s
    };
    Ok(format!("{}\nvoid vs(){{resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;}}\nvoid ps(){{\n{}\n}}",convert(&globals),convert(body)))
}

#[cfg(test)] mod tests {
    use super::*;
    const SRC:&str="precision mediump float; varying vec2 resultCoord1; uniform sampler2D textureFore; uniform float alpha; uniform float weights[8]; const vec3 graydata=vec3(0.3,0.6,0.1); void main(){vec4 fore=texture2D(textureFore,resultCoord1)*weights[0]; fore.rgb=vec3(dot(fore.rgb,graydata)+0.2); fore.a*=alpha; gl_FragColor=fore;}";
    #[test] fn mobile_and_console_preserve_formula_and_layout(){
        for source in [SRC.to_string(),SRC.replace("precision mediump float;", ""),format!("#version 100\n{SRC}")] {
            let (cg,uniforms)=super::super::hlsl::translate_cg_effect(source.as_bytes()).unwrap();
            assert!(cg.contains("dot(fore.rgb,graydata)+0.2"));
            assert!(cg.contains("static const float3 graydata"));
            assert!(cg.contains("tex2D(samplerFore,texCoord1)*weights[0]"));
            assert_eq!(uniforms.iter().map(|u|(u.name.as_str(),u.offset,u.count)).collect::<Vec<_>>(),[("alpha",0,1),("weights",1,8)]);
        }
    }
    #[test] fn unsupported_semantics_fail_closed(){
        for source in [SRC.replace("texture2D(textureFore", "texture2D(textureBack"),
            SRC.replace("varying vec2 resultCoord1", "varying vec3 customCoord"),
            format!("#define ALPHA 1\n{SRC}"),SRC.replace("void main()", "void main(float x)"),
            SRC.replace("uniform float alpha", "uniform mat4 alpha")] {
            assert!(super::super::hlsl::translate_cg_effect(source.as_bytes()).is_err());
        }
        assert!(to_hlsl(&[0xff,0,1]).is_err());
    }
    #[test] fn unused_helper_and_legacy_comments_do_not_change_executable_source(){
        let helper="uniform float helper(uniform float a, uniform float b){return a+b;}";
        let source=SRC.replace("void main",&format!("{helper} void main"));
        assert_eq!(to_hlsl(source.as_bytes()).unwrap(),to_hlsl(SRC.as_bytes()).unwrap());
        assert!(to_hlsl(source.replace("fore.a*=alpha","fore.a*=helper(alpha,alpha)").as_bytes()).is_err());
        let mut bytes=b"// legacy comment: \xff\xfe\n".to_vec();bytes.extend_from_slice(SRC.as_bytes());
        assert_eq!(to_hlsl(&bytes).unwrap(),to_hlsl(SRC.as_bytes()).unwrap());
    }
}

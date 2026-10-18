//! Fixed DX11 pixel wrapper normalization. Destination reads remain unsupported;
//! only demonstrably unused local reads may be removed.
use super::hlsl::{matching_brace,strip_comments};
fn compact(s:&str)->String{s.chars().filter(|c|!c.is_whitespace()).collect()}
pub fn to_hlsl(data:&[u8])->Result<String,String>{
    let source=strip_comments(&String::from_utf8_lossy(data));
    let at=source.find("float4 main").ok_or("missing DX11 pixel entry")?;
    let open=at+source[at..].find('{').ok_or("missing DX11 body")?;
    if compact(&source[at..open])!="float4main(SVertexShaderResultvertexShaderResult):SV_TARGET"{
        return Err("custom DX11 signature requires a separate port".into());
    }
    let close=matching_brace(&source,open).ok_or("unbalanced DX11 body")?;
    if !source[close+1..].trim().is_empty(){return Err("extra DX11 stages unsupported".into());}
    let mut prefix=source[..at].trim();let mut globals=String::new();let mut vertex=false;
    while !prefix.is_empty(){
        if prefix.starts_with("struct ")||prefix.starts_with("cbuffer "){
            let open=prefix.find('{').ok_or("missing DX11 declaration block")?;
            let close=matching_brace(prefix,open).ok_or("unbalanced DX11 declaration")?;
            if prefix.starts_with("struct ") {
                if compact(&prefix[..open])!="structSVertexShaderResult"||vertex
                    ||compact(&prefix[open+1..close])!="float4position:SV_POSITION;float2texCoord0:TEXCOORD0;float2texCoord1:TEXCOORD1;"{
                    return Err("custom DX11 vertex interface unsupported".into());
                }vertex=true;
            }else{globals.push_str(&prefix[open+1..close]);globals.push('\n');}
            prefix=prefix[close+1..].trim_start().strip_prefix(';').ok_or("DX11 block terminator")?.trim_start();
        }else{
            let end=prefix.find(';').ok_or("DX11 resource terminator")?;
            let resource=compact(&prefix[..end]);
            if !matches!(resource.as_str(),"SamplerStatesamplerBack:register(s0)"|
                "Texture2DtextureBack:register(t0)"|"Texture2DtextureFore:register(t1)"|
                "Texture2DtextureMask:register(t2)"|"Texture2DtextureUser:register(t3)"){
                return Err(format!("unsupported DX11 resource: {resource}"));
            }prefix=prefix[end+1..].trim_start();
        }
    }
    if !vertex{return Err("missing DX11 vertex interface".into());}
    let mut body=source[open+1..close].to_string();
    for (texture,sampler) in [("textureBack","samplerBack"),("textureFore","samplerFore"),("textureMask","samplerMask"),("textureUser","samplerUser")]{
        body=body.replace(&format!("{texture}.Sample(samplerBack,"),&format!("tex2D({sampler},"));
    }
    body=body.replace("vertexShaderResult.texCoord0","texCoord0").replace("vertexShaderResult.texCoord1","texCoord1");
    if body.contains(".Sample")||body.contains("vertexShaderResult."){return Err("unsupported DX11 sample/interface access".into());}
    // These wrappers often load back without using it. Removing that exact
    // side-effect-free declaration does not require a destination snapshot.
    let tokens:Vec<_>=body.split(|c:char|!c.is_ascii_alphanumeric()&&c!='_').collect();
    if tokens.iter().filter(|&&t|t=="back").count()==1 {
        if let Some(start)=body.find("float4 back"){
            if let Some(end)=body[start..].find(';').map(|i|start+i){
                if compact(&body[start..=end])=="float4back=tex2D(samplerBack,texCoord0);"{body.replace_range(start..=end,"");}
            }
        }
    }
    let returns=body.split(|c:char|!c.is_ascii_alphanumeric()&&c!='_').filter(|&t|t=="return").count();
    let ret=body.rfind("return ").ok_or("missing DX11 result")?;
    let expr=body[ret+7..].trim().strip_suffix(';').ok_or("DX11 result terminator")?.trim();
    if returns!=1||expr.contains(';'){return Err("early DX11 returns unsupported".into());}
    body=format!("{}result = {};",&body[..ret],expr);
    Ok(format!("{globals}\nvoid vs(){{resultPosition=position;resultTexCoord0=texCoord0;resultTexCoord1=texCoord1;}}\nvoid ps(){{{body}}}"))
}
#[cfg(test)]mod tests{
 use super::*;
 const SRC:&str="struct SVertexShaderResult{float4 position:SV_POSITION;float2 texCoord0:TEXCOORD0;float2 texCoord1:TEXCOORD1;};cbuffer ConstantBuffer{float alpha;};SamplerState samplerBack:register(s0);Texture2D textureBack:register(t0);Texture2D textureFore:register(t1);float4 main(SVertexShaderResult vertexShaderResult):SV_TARGET{float4 back = textureBack.Sample(samplerBack, vertexShaderResult.texCoord0);float4 fore=textureFore.Sample(samplerBack, vertexShaderResult.texCoord1);fore.a*=alpha;return fore;}";
 #[test]fn unused_destination_read_is_removed_but_live_reads_fail(){
  let (cg,u)=super::super::hlsl::translate_cg_effect(SRC.as_bytes()).unwrap();
  assert!(!cg.contains("samplerBack"));assert!(cg.contains("result = fore;"));assert_eq!(u.len(),1);
  assert!(super::super::hlsl::translate_cg_effect(SRC.replace("return fore;","return fore+back;").as_bytes()).is_err());
  assert!(to_hlsl(SRC.replace("SV_POSITION","CUSTOM_POSITION").as_bytes()).is_err());
 }
}

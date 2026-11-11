local select, assert, type, tostring, ipairs, unpack, error, setmetatable = select, assert, type, tostring, ipairs, unpack, error, setmetatable
local is_save, read_host, write_host = ...
local native_open, native_close, native_type = io.open, io.close, io.type
local handles = setmetatable({}, {__mode='k'})
local methods = {}
local function checked(f)
    assert(handles[f], 'invalid file handle')
    assert(not f.closed, 'attempt to use a closed file')
end
function methods:flush()
    checked(self)
    if self.dirty then
        local ok, err = write_host(self.path, self.data)
        if not ok then return nil, err end
        self.dirty = false
    end
    return true
end
function methods:close()
    checked(self)
    local ok, err = self:flush()
    if not ok then return nil, err end
    self.closed = true
    return true
end
function methods:write(...)
    checked(self)
    if not self.writable then return nil, 'file is not writable' end
    for i=1,select('#',...) do
        local v=select(i,...)
        assert(type(v)=='string' or type(v)=='number', 'string or number expected')
        v=tostring(v)
        if self.append then self.pos=#self.data end
        if self.pos>#self.data then self.data=self.data..string.rep('\0',self.pos-#self.data) end
        self.data=self.data:sub(1,self.pos)..v..self.data:sub(self.pos+#v+1)
        self.pos=self.pos+#v
    end
    self.dirty=true
    return self
end
function methods:seek(whence, offset)
    checked(self)
    whence,offset=whence or 'cur',offset or 0
    local base=({set=0,cur=self.pos,['end']=#self.data})[whence]
    if not base or base+offset<0 then return nil,'invalid seek' end
    self.pos=base+offset
    return self.pos
end
function methods:read(...)
    checked(self)
    if not self.readable then return nil,'file is not readable' end
    local args={...};if #args==0 then args={'*l'} end
    local result={}
    for i,fmt in ipairs(args) do
        local value
        if fmt=='*a' or fmt=='*all' then value=self.data:sub(self.pos+1);self.pos=#self.data
        elseif type(fmt)=='number' and fmt>=0 then
            if self.pos<#self.data then value=self.data:sub(self.pos+1,self.pos+fmt);self.pos=self.pos+#value end
        elseif fmt=='*l' then
            if self.pos<#self.data then
                local ending=self.data:find('\n',self.pos+1,true)
                value=self.data:sub(self.pos+1,ending and ending-1 or #self.data)
                self.pos=ending or #self.data
            end
        else error('unsupported read format: '..tostring(fmt)) end
        result[i]=value
        if value==nil then return unpack(result,1,i) end
    end
    return unpack(result,1,#args)
end
function methods:lines()
    checked(self)
    return function() return self:read('*l') end
end
io.open=function(path,mode)
    if not is_save(path) then return native_open(path,mode) end
    mode=mode or 'r'
    if not ({r=true,rb=true,w=true,wb=true,a=true,ab=true,['r+']=true,['rb+']=true,['r+b']=true,
        ['w+']=true,['wb+']=true,['w+b']=true,['a+']=true,['ab+']=true,['a+b']=true})[mode] then
        return nil,'invalid file mode'
    end
    local first=mode:sub(1,1)
    local data,err='',nil
    if first~='w' then
        data,err=read_host(path)
        if not data and first=='r' then return nil,err end
        data=data or ''
    end
    if first=='w' or first=='a' then
        local ok;ok,err=write_host(path,data)
        if not ok then return nil,err end
    end
    local f=setmetatable({path=path,data=data,pos=first=='a' and #data or 0,
        readable=first=='r' or mode:find('+',1,true)~=nil,
        writable=first~='r' or mode:find('+',1,true)~=nil,append=first=='a'}, {__index=methods})
    handles[f]=true
    return f
end
io.close=function(f) if handles[f] then return f:close() end return native_close(f) end
io.type=function(f) if handles[f] then return f.closed and 'closed file' or 'file' end return native_type(f) end

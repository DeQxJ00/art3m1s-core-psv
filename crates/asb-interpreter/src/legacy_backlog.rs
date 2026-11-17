//! Keep the script-owned backlog contiguous across waits and older saves.

const PATCH: &[u8] = br#"
do
    local finish_row = set_backlog_next
    function set_backlog_next()
        if log.stack and log.stack[log.count] then
            return finish_row()
        end
    end

    local open_backlog = backlog_init
    function backlog_init()
        local stack = log.stack
        if type(stack) == 'table' then
            local last = table.maxn(stack)
            local has_pending_row = log.count == last and last > 0
            local next_slot = 1
            for index = 1, last do
                local row = stack[index]
                if row then
                    stack[next_slot] = row
                    next_slot = next_slot + 1
                end
            end
            for index = next_slot, last do stack[index] = nil end
            log.count = has_pending_row and next_slot - 1 or next_slot
        end
        return open_backlog()
    end
end
"#;

pub(crate) fn repair_source(path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    if path.replace('\\', "/").to_ascii_lowercase() != "system/ui/backlog.lua"
        || bytes.ends_with(PATCH)
    {
        return None;
    }
    let markers: &[&[u8]] = &[
        b"function backlog_init()",
        b"function set_backlog_next()",
        b"if not log.stack[log.count] then",
        b"log.count >= tonumber(init.backlog_max)",
        b"pairs(tbl.text) do e:tag(val) end",
    ];
    if !markers.iter().all(|marker| bytes.windows(marker.len()).any(|part| part == *marker)) {
        return None;
    }
    // Append ASCII so the original source encoding and line numbers survive.
    Some([bytes, PATCH].concat())
}

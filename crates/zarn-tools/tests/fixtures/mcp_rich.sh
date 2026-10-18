#!/bin/sh
grown=0
while IFS= read -r line; do
    id=$(printf '%s' "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
    case "$line" in
    *'"initialize"'*)
        printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{"listChanged":true},"resources":{},"prompts":{}}}}\n' "$id"
        ;;
    *'"tools/list"'*)
        if [ "$grown" = "1" ]; then
            printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"grow","description":"adds a tool","inputSchema":{"type":"object"}},{"name":"fresh","description":"appeared later","inputSchema":{"type":"object"}}]}}\n' "$id"
        else
            printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"grow","description":"adds a tool","inputSchema":{"type":"object"}}]}}\n' "$id"
        fi
        ;;
    *'"tools/call"'*)
        grown=1
        printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"grown"}]}}\n' "$id"
        printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n'
        ;;
    *'"resources/list"'*)
        printf '{"jsonrpc":"2.0","id":%s,"result":{"resources":[{"uri":"docs://readme","name":"readme","description":"the readme","mimeType":"text/markdown"}]}}\n' "$id"
        ;;
    *'"resources/read"'*)
        case "$line" in
        *docs://readme*) printf '{"jsonrpc":"2.0","id":%s,"result":{"contents":[{"uri":"docs://readme","mimeType":"text/markdown","text":"# Readme from MCP"}]}}\n' "$id" ;;
        *) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32002,"message":"resource not found"}}\n' "$id" ;;
        esac
        ;;
    *'"prompts/list"'*)
        printf '{"jsonrpc":"2.0","id":%s,"result":{"prompts":[{"name":"summarize","description":"summarize a topic","arguments":[{"name":"topic","required":true},{"name":"style"}]}]}}\n' "$id"
        ;;
    *'"prompts/get"'*)
        topic=$(printf '%s' "$line" | grep -o '"topic":"[^"]*"' | cut -d'"' -f4)
        style=$(printf '%s' "$line" | grep -o '"style":"[^"]*"' | cut -d'"' -f4)
        printf '{"jsonrpc":"2.0","id":%s,"result":{"messages":[{"role":"user","content":{"type":"text","text":"Summarize %s in a %s style."}}]}}\n' "$id" "$topic" "$style"
        ;;
    esac
done

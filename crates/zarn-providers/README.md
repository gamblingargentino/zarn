# zarn-providers

Provider implementations for various backends.

## Exec Provider

The `exec` provider spawns a configured command as a subprocess and communicates with it via JSON over stdin/stdout. This is the escape hatch for any backend without a dedicated adapter.

### Configuration

Add an `exec` provider to your config:

```toml
[[providers]]
kind = "exec"
command = "/path/to/adapter"
args = "arg1,arg2,arg3"
context_window = 128000
max_output = 4096
vision = false
tools = "native"
```

- `command`: Path to the executable (required)
- `args`: Comma-separated arguments to pass to the command (optional)
- `context_window`, `max_output`, `vision`, `tools`: Standard capability overrides (optional)

### Wire Format

**Request (stdin):** A single line containing the Request serialized as JSON, followed by a newline.

```json
{"messages":[{"role":"user","content":"Hello"}],"tools":[],"model":"gpt-4","max_tokens":4096,"temperature":1.0}
```

**Events (stdout):** One Event per line, each a complete JSON object, followed by a newline.

```json
{"TextDelta":"Hello, "}
{"TextDelta":"world!"}
{"Done":"EndTurn"}
```

### Event Schema

The adapter must emit events from `zarn_core::provider::Event`:

- `TextDelta(String)`: Incremental text output
- `ToolCallStart { id: String, name: String }`: Start of a tool invocation
- `ToolCallDelta(String)`: Incremental tool arguments (JSON)
- `ToolCallEnd`: End of tool arguments
- `Usage { input: u32, output: u32, cached: u32 }`: Token counts
- `Done(StopReason)`: Completion; reason is `EndTurn`, `ToolUse`, `MaxTokens`, `Cancelled`, or `Refusal`

`Event` is an externally tagged enum. Unit variants are bare strings, tuple
variants wrap their payload, and struct variants nest an object. Every line
must be one of these shapes exactly:

```json
{"TextDelta": "Hello, "}
{"ToolCallStart": {"id": "call_1", "name": "read"}}
{"ToolCallDelta": "{\"path\":"}
"ToolCallEnd"
{"Usage": {"input": 1200, "output": 83, "cached": 0}}
{"Done": "EndTurn"}
{"Error": {"RateLimit": "slow down"}}
```

`ToolCallEnd` is the one event that is a bare JSON string, not an object.

Send exactly one `Done` when the turn is finished. If the process exits
without one, zarn reports an error carrying the exit code and anything the
adapter wrote to stderr. Lines that do not parse are logged and skipped, so a
stray debug print will not kill the stream -- but write diagnostics to stderr,
not stdout.
- `Error(ProviderError)`: Error during processing

### Request Schema

The adapter receives a Request:

```json
{
  "messages": [
    {
      "System": "You are an assistant"
    },
    {
      "User": [
        {
          "Text": "Hello"
        }
      ]
    },
    {
      "Assistant": {
        "content": [{"Text": "Hi there"}],
        "tool_calls": []
      }
    },
    {
      "ToolResult": {
        "id": "tool-123",
        "content": "Tool output",
        "is_error": false
      }
    }
  ],
  "tools": [
    {
      "name": "get_weather",
      "description": "Get the current weather",
      "schema": { "type": "object", "properties": {...} }
    }
  ],
  "model": "gpt-4",
  "max_tokens": 4096,
  "temperature": 1.0
}
```

### Example Adapter (Python)

```python
#!/usr/bin/env python3
import json
import sys

while True:
    line = sys.stdin.readline()
    if not line:
        break
    
    request = json.loads(line)
    
    # Process request.messages, request.tools, etc.
    # ...
    
    # Stream events
    print(json.dumps({"TextDelta": "Response text"}), flush=True)
    print(json.dumps({"Done": "EndTurn"}), flush=True)
```

### Example Adapter (Rust)

```rust
use serde_json::{json, from_str};
use zarn_core::types::Request;
use zarn_core::provider::Event;
use zarn_core::types::StopReason;
use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    
    for line in stdin.lock().lines() {
        let request: Request = from_str(&line.unwrap()).unwrap();
        
        // Process request
        // ...
        
        writeln!(stdout, "{}", json!(Event::TextDelta("Response".to_string()))).unwrap();
        writeln!(stdout, "{}", json!(Event::Done(StopReason::EndTurn))).unwrap();
    }
}
```

## In-process Llama.cpp

The optional `local` feature enables `LlamaCppProvider`, which loads a GGUF
model in-process using `llama-cpp-2`. Enable it with
`zarn-providers = { path = "...", features = ["local"] }`. Configure a model
file with `model_path`:

```toml
[providers.local]
kind = "llamacpp"
model_path = "models/model.gguf"
```

The native llama.cpp dependency is built from source and requires a working
native build toolchain.

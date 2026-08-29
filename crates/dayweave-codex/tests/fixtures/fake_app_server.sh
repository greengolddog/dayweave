#!/bin/sh
set -eu

fail() {
    printf '%s\n' 'fake app-server contract failure' >&2
    exit "$1"
}

expect_contains() {
    case "$1" in
        *"$2"*) ;;
        *) fail "$3" ;;
    esac
}

[ "$#" -eq 2 ] || fail 10
[ "$1" = 'app-server' ] || fail 11
[ "$2" = '--stdio' ] || fail 12
[ "${PWD-}" = "${CODEX_HOME-}" ] || fail 13
[ "${LANG-}" = 'C' ] || fail 14
[ -n "${TMPDIR-}" ] || fail 18
[ -z "${CARGO_MANIFEST_DIR+x}" ] || fail 15
[ -z "${HOME+x}" ] || fail 16

printf '%s\n' 'fake-stderr-secret' >&2

record() {
    printf '%s\n' "$1" >> "$TMPDIR/transcript.jsonl"
}

IFS= read -r message || fail 20
record "$message"
expect_contains "$message" '"id":1' 21
expect_contains "$message" '"method":"initialize"' 22
expect_contains "$message" '"name":"dayweave"' 23
printf '{"id":1,"result":{"userAgent":"fake/1","platformFamily":"unix","platformOs":"test","codexHome":"%s"}}\n' "$CODEX_HOME"

IFS= read -r message || fail 24
record "$message"
expect_contains "$message" '"method":"initialized"' 25

if [ -f "$TMPDIR/exit-flow" ]; then
    exit 17
fi

if [ -f "$TMPDIR/api-key-flow" ]; then
    IFS= read -r message || fail 30
    expect_contains "$message" '"id":2' 31
    expect_contains "$message" '"method":"account/login/start"' 32
    expect_contains "$message" '"type":"apiKey"' 33
    expect_contains "$message" '"apiKey":"test-api-key-secret"' 34
    printf '%s\n' '{"id":2,"result":{"type":"chatgpt"}}'
    if IFS= read -r message; then
        fail 35
    fi
    exit 0
fi

if [ -f "$TMPDIR/timeout-flow" ]; then
    IFS= read -r message || fail 40
    expect_contains "$message" '"method":"account/read"' 41
    (/bin/sleep 1; printf '%s\n' 'escaped' > "$TMPDIR/grandchild-survived") &
    /bin/sleep 5
    exit 0
fi

if [ -f "$TMPDIR/drop-flow" ]; then
    (/bin/sleep 1; printf '%s\n' 'escaped' > "$TMPDIR/grandchild-survived") &
    IFS= read -r message || exit 0
    exit 0
fi

if [ -f "$TMPDIR/clean-exit-grandchild-flow" ]; then
    (/bin/sleep 1; printf '%s\n' 'escaped' > "$TMPDIR/grandchild-survived") >/dev/null 2>&1 &
    exit 0
fi

if [ -f "$TMPDIR/wrong-id-flow" ]; then
    IFS= read -r message || fail 50
    expect_contains "$message" '"method":"account/read"' 51
    printf '%s\n' '{"id":99,"result":{"account":null,"requiresOpenaiAuth":true}}'
    /bin/sleep 5
    exit 0
fi

if [ -f "$TMPDIR/oversize-flow" ]; then
    IFS= read -r message || fail 110
    expect_contains "$message" '"method":"account/read"' 111
    printf '%s' '{"id":2,"result":{"padding":"'
    index=0
    while [ "$index" -lt 2048 ]; do
        printf '%s' 'x'
        index=$((index + 1))
    done
    printf '%s\n' '"}}'
    /bin/sleep 5
    exit 0
fi

if [ -f "$TMPDIR/null-login-flow" ]; then
    IFS= read -r message || fail 112
    expect_contains "$message" '"method":"account/read"' 113
    printf '%s\n' '{"method":"account/login/completed","params":{"loginId":null,"success":true,"error":null}}'
    /bin/sleep 5
    exit 0
fi

if [ -f "$TMPDIR/auth-flow" ]; then
    IFS= read -r message || fail 52
    expect_contains "$message" '"id":2' 53
    expect_contains "$message" '"method":"account/login/start"' 54
    expect_contains "$message" '"type":"chatgpt"' 55
    expect_contains "$message" '"useHostedLoginSuccessPage":true' 56
    printf '%s\n' '{"method":"account/login/completed","params":{"loginId":"browser-login","success":true,"error":null}}'
    printf '%s\n' '{"id":2,"result":{"type":"chatgpt","loginId":"browser-login","authUrl":"https://example.test/login"}}'

    IFS= read -r message || fail 57
    expect_contains "$message" '"id":3' 58
    expect_contains "$message" '"method":"account/login/start"' 59
    expect_contains "$message" '"type":"chatgptDeviceCode"' 100
    printf '%s\n' '{"id":3,"result":{"type":"chatgptDeviceCode","loginId":"device-login","verificationUrl":"https://example.test/device","userCode":"ABCD-EFGH"}}'
    printf '%s\n' '{"method":"account/login/completed","params":{"loginId":"device-login","success":true,"error":null}}'

    IFS= read -r message || fail 101
    expect_contains "$message" '"id":4' 102
    expect_contains "$message" '"method":"account/logout"' 103
    case "$message" in
        *'"params"'*) fail 104 ;;
        *) ;;
    esac
    printf '%s\n' '{"id":4,"result":{}}'
    if IFS= read -r message; then
        fail 105
    fi
    exit 0
fi


IFS= read -r message || fail 60
record "$message"
expect_contains "$message" '"id":2' 61
expect_contains "$message" '"method":"account/read"' 62
expect_contains "$message" '"refreshToken":false' 63
printf '%s\n' '{"id":2,"result":{"account":{"type":"chatgpt","email":"user@example.test","planType":"plus"},"requiresOpenaiAuth":true}}'

IFS= read -r message || fail 64
record "$message"
expect_contains "$message" '"id":3' 65
expect_contains "$message" '"method":"thread/start"' 66
expect_contains "$message" '"approvalPolicy":"never"' 67
expect_contains "$message" '"sandbox":"read-only"' 68
expect_contains "$message" '"serviceName":"dayweave"' 69
printf '%s\n' '{"id":3,"result":{"thread":{"id":"thread-1"}}}'

IFS= read -r message || fail 70
record "$message"
expect_contains "$message" '"id":4' 71
expect_contains "$message" '"method":"thread/resume"' 72
expect_contains "$message" '"threadId":"thread-1"' 73
expect_contains "$message" '"approvalPolicy":"never"' 74
expect_contains "$message" '"sandbox":"read-only"' 75
if [ -f "$TMPDIR/resume-mismatch-flow" ]; then
    printf '%s\n' '{"id":4,"result":{"thread":{"id":"different-thread"}}}'
    /bin/sleep 5
    exit 0
fi
printf '%s\n' '{"id":4,"result":{"thread":{"id":"thread-1"}}}'

IFS= read -r message || fail 76
record "$message"
expect_contains "$message" '"id":5' 77
expect_contains "$message" '"method":"turn/start"' 78
expect_contains "$message" '"threadId":"thread-1"' 79
expect_contains "$message" '"approvalPolicy":"never"' 106
expect_contains "$message" '"sandboxPolicy":{"type":"readOnly","networkAccess":false,"access":{"type":"restricted","includePlatformDefaults":false,"readableRoots":[' 107
expect_contains "$message" '"outputSchema":{' 108
expect_contains "$message" '"text":"private planner prompt"' 109
if [ -f "$TMPDIR/bad-turn-start-flow" ]; then
    (/bin/sleep 1; printf '%s\n' 'escaped' > "$TMPDIR/grandchild-survived") &
    : > "$TMPDIR/fatal-response-sent"
    printf '%s\n' '{"id":5,"result":{"turn":{"id":"turn-1","status":"completed","items":[]}}}'
    /bin/sleep 5
    exit 0
fi
printf '%s\n' '{"id":5,"result":{"turn":{"id":"turn-1","status":"inProgress","items":[]}}}'

if [ -f "$TMPDIR/queued-overflow-flow" ]; then
    printf '%s' '{"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","item":{"type":"agentMessage","text":"'
    index=0
    while [ "$index" -lt 1024 ]; do
        printf '%s' 'x'
        index=$((index + 1))
    done
    printf '%s\n' '"}}}'
    /bin/sleep 5
    exit 0
fi

printf '%s\n' '{"id":"command-approval","method":"item/commandExecution/requestApproval","params":{"command":["false"]}}'
IFS= read -r message || fail 80
record "$message"
expect_contains "$message" '"id":"command-approval"' 81
expect_contains "$message" '"result":{"decision":"decline"}' 82

printf '%s\n' '{"id":"file-approval","method":"item/fileChange/requestApproval","params":{}}'
IFS= read -r message || fail 83
record "$message"
expect_contains "$message" '"id":"file-approval"' 84
expect_contains "$message" '"result":{"decision":"decline"}' 85

printf '%s\n' '{"id":"permissions-approval","method":"item/permissions/requestApproval","params":{}}'
IFS= read -r message || fail 86
record "$message"
expect_contains "$message" '"id":"permissions-approval"' 87
expect_contains "$message" '"result":{"permissions":{}}' 88

printf '%s\n' '{"id":"mcp-elicitation","method":"mcpServer/elicitation/request","params":{}}'
IFS= read -r message || fail 89
record "$message"
expect_contains "$message" '"id":"mcp-elicitation"' 90
expect_contains "$message" '"result":{"action":"decline","content":null}' 91

if [ -f "$TMPDIR/user-input-flow" ]; then
    printf '%s\n' '{"id":"user-input","method":"item/tool/requestUserInput","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"item-user-input","isBlocking":true,"questions":[{"id":"approval","header":"Approval","question":"Run it?","options":[{"label":"Accept","description":"Runs it"}]}]}}'
    IFS= read -r message || fail 92
    record "$message"
    expect_contains "$message" '"id":"user-input"' 119
    expect_contains "$message" '"code":-32601' 118
    case "$message" in
        *'"answers"'*) fail 117 ;;
        *) ;;
    esac
    IFS= read -r message || fail 120
    record "$message"
    expect_contains "$message" '"method":"turn/interrupt"' 121
    printf '%s\n' '{"id":6,"result":{}}'
    /bin/sleep 5
    exit 0
fi

printf '%s\n' '{"id":"legacy-exec","method":"execCommandApproval","params":{}}'
IFS= read -r message || fail 123
record "$message"
expect_contains "$message" '"result":{"decision":"abort"}' 124

printf '%s\n' '{"id":"legacy-patch","method":"applyPatchApproval","params":{}}'
IFS= read -r message || fail 125
record "$message"
expect_contains "$message" '"result":{"decision":"abort"}' 126

printf '%s\n' '{"id":"unknown-request","method":"unknown/approval","params":{"secret":"do-not-reflect"}}'
IFS= read -r message || fail 95
record "$message"
expect_contains "$message" '"id":"unknown-request"' 96
expect_contains "$message" '"code":-32601' 97
expect_contains "$message" '"message":"Client denies server-initiated requests"' 98

printf '%s\n' '{"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","completedAtMs":1,"item":{"type":"agentMessage","id":"item-1","text":"{\"answer\":42}"}}}'
printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[]}}}'

if IFS= read -r message; then
    fail 99
fi

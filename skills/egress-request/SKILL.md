---
name: egress-request
description: Use when an HTTP or HTTPS request from this VM fails with status 403 from the proxy, or when a tool reports that the proxy refused a host. Asks the user to allow the request.
---

# egress-request

This VM reaches the network only through a proxy (the gatekeeper). The
gatekeeper allows only some hosts. It refuses all other requests with
status 403. Credentials are not in the VM: the gatekeeper adds them to
the requests that it allows.

## When the gatekeeper refuses a request

1. Make sure that the task needs this host. Do not ask for a host that
   the task does not need.
2. Run `egress-request` with the host, method and path of the refused
   request, and the reason:

   ```sh
   egress-request api.example.org --method GET --path '/v1/items/*' \
     --reason "The tests download the item list from api.example.org"
   ```

   - Use the same host, method and path as the refused request. The
     broker accepts a request only if the gatekeeper refused a matching
     request in the last 15 minutes.
   - A `*` at the end of the path matches all paths below it.
   - The reason is for the user. Write one clear sentence.
3. The command blocks until the user decides, for at most 30 minutes.
   Do not stop it.
4. Read the exit status:
   - `0`: the user allowed the request. Make the request again.
   - `1`: the user denied the request. Do not make it again. Find another
     way, or tell the user that the task cannot continue.
   - `2`: an error. Read the message on stderr.

## Rules

- Never ask for a credential or a secret. Never put a secret in the
  reason.
- Do not try to go around the proxy. Other connections are blocked.
- Do not unset `HTTP_PROXY`, `HTTPS_PROXY` or the CA variables.

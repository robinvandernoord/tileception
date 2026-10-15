# tileception-auth

Standalone Linux-PAM password verification for the compositor's lock screen.
This crate is deliberately outside the repository workspace.

`check_password(user: &str, password: SecretString) -> Result<Outcome, Error>`
blocks through `pam_authenticate` and then `pam_acct_mgmt`. Only `Success`
permits unlocking. `WrongPassword`, `AccountProblem { code, message }`, and
`TryAgainLater { code, message }` distinguish denials. Unexpected PAM failures
are errors and must keep the screen locked. Messages come from `pam_strerror`,
never from module conversation text. Account expiration and required password
changes deny access; this library never changes passwords, credentials, or
sessions.

For the compositor, use:

```rust,no_run
use tileception_auth::{Authenticator, SecretString};
let (auth, results) = Authenticator::start(|| {
    // Ping the compositor's event loop here.
}).unwrap();
auth.submit("logged-in-user".into(), SecretString::from("example only")).unwrap();
// In the event-loop wake handler, drain results.try_recv().
```

The worker runs all PAM calls on one dedicated thread. Submit never waits for
PAM; attempts arriving while busy are rejected and their secrets dropped.
Results enter the channel before `wake` runs, and `wake` must be quick and must
not panic. Dropping the authenticator closes its request channel without waiting
for an ongoing attempt. That attempt still finishes, cleans up PAM, and may
deliver one final result and wake. Dropping the receiver discards subsequent
results. The worker cannot cancel a blocked PAM module, and should normally
live for the compositor's lifetime. A thread avoids unsafe post-fork Rust and
PAM activity in a multithreaded compositor; it provides no process isolation.

The caller must supply the actual logged-in user's identity from trusted
session state, never an editable username. This assumes an ordinary unprivileged
Linux login using `pam_unix` and its privileged `unix_chkpwd` helper for that same
user. It is not a facility for checking other users' passwords. Site-specific
PAM modules can impose other privilege, terminal, or interaction requirements.

The service is `tileception`, falling back to `login` only when
`/etc/pam.d/tileception` does not exist. Other metadata errors fail closed.
The service in `pam/tileception` includes the Debian/Ubuntu login authentication
and account stacks explicitly. `install-session.sh` installs it as
`/etc/pam.d/tileception`; this crate performs no installation itself. Review
site-specific login policy before deployment. Do not remove `pam_acct_mgmt`
to accommodate an account-policy denial.

Passwords use `secrecy::SecretString`. The original is dropped immediately after
authentication, before account management. The conversation supplies the user
for echo-on prompts and the password for each echo-off prompt, so this supports
password stacks, not multi-factor challenges or password changes. Info/error
messages are acknowledged without retaining or displaying arbitrary module
text, which might expose secrets. Unsupported styles fail closed. Responses
are allocated with the C allocator: partial responses are wiped and freed on
failure; successful responses belong to PAM, which must wipe/free its copies.
The library cannot wipe PAM-owned storage early without invalidating pointers.

This protects against accidental Rust-side password logging and retaining the
owned password after an attempt. It does not protect against compromised PAM
modules, the compositor, privileged attackers, process memory inspection,
swap/core dumps, or a malicious configuration. The UI must clear its own input
buffers and must not print secrets. PAM can hide a locked account behind the
same authentication failure as a bad password. Failure delays block the worker;
they do not necessarily produce a distinct status. Temporary/unavailable and
maximum-attempt statuses produce `TryAgainLater`, with no inferred retry time.
Never automatically resubmit, particularly after `PAM_MAXTRIES`.

## Build and verification

Install the distribution's `libpam0g-dev` development package through your normal
system administration process. No dependencies are installed by this crate.

```sh
cd crates/tileception-auth
cargo test
cargo clippy -- -D warnings
cargo fmt --check
cargo run --example check -- "$USER"
```

The manual example reads a password from stdin with terminal echo disabled;
never pass it on the command line. Tests use only synthetic secrets. They cover
conversation response ownership, rejection and partial-response cleanup,
outcome mapping, invalid inputs, worker busy rejection, result delivery, and
wake ordering. The private-confdir test dynamically checks for the optional
Linux-PAM 1.4+ `pam_start_confdir` symbol, then uses throwaway permit/deny
policies, including account denial, without writing to `/etc`. It prints a skip
reason on older PAM. The production path always uses `pam_start`.

FFI layouts and ownership follow upstream Linux-PAM's
[headers](https://github.com/linux-pam/linux-pam/blob/master/libpam/include/security/_pam_types.h)
and [conversation documentation](https://github.com/linux-pam/linux-pam/blob/master/doc/man/pam_conv.3.xml).
Status handling follows its
[authentication](https://github.com/linux-pam/linux-pam/blob/master/doc/man/pam_authenticate.3.xml)
and [account](https://github.com/linux-pam/linux-pam/blob/master/doc/man/pam_acct_mgmt.3.xml)
documentation.

## Remaining uncertainties

The development environment now has the PAM development dependencies. All six
tests pass, including private permit/deny policies and account denial.
Formatting and Clippy checks pass too.
The maintainer must verify its deployed login/account policy and same-user
helper behavior. PAM has no portable way to distinguish every lockout from an
incorrect password or to discover a lockout timeout, and this library does not
guess. Response wiping after successful ownership transfer depends on the
installed PAM modules. No real-password or full compositor unlock test was run.

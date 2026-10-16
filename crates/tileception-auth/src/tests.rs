use super::*;
use std::time::Duration;

fn secret() -> SecretString {
    SecretString::from("synthetic-test-token")
}

#[test]
#[allow(unsafe_code, reason = "this test exercises the PAM callback's C response allocation and cleanup")]
fn conversation_answers_all_styles_and_repeated_prompts() {
    let mut data = Conversation { user: CString::new("test-user").unwrap(), password: Some(secret()) };
    let messages = [1, 2, 3, 4, 1].map(|style| PamMessage { style, text: ptr::null() });
    let mut pointers = messages.each_ref().map(ptr::from_ref);
    let mut output = ptr::null_mut();
    // SAFETY: All pointers refer to live test data and successful responses are freed below.
    unsafe {
        assert_eq!(converse(5, pointers.as_mut_ptr(), &mut output, ptr::from_mut(&mut data).cast()), SUCCESS);
        for (index, expected) in [(0, "synthetic-test-token"), (1, "test-user"), (4, "synthetic-test-token")] {
            assert_eq!(CStr::from_ptr((*output.add(index)).text).to_str().unwrap(), expected);
            assert_eq!((*output.add(index)).code, 0);
        }
        assert!((*output.add(2)).text.is_null());
        assert!((*output.add(3)).text.is_null());
        clear_responses(output, 5);
    }
}

#[test]
#[allow(unsafe_code, reason = "this test exercises the PAM callback's handling of invalid C messages")]
fn conversation_rejects_invalid_messages_and_unavailable_password() {
    let mut data = Conversation { user: CString::new("test-user").unwrap(), password: Some(secret()) };
    let messages = [1, 99].map(|style| PamMessage { style, text: ptr::null() });
    let mut pointers = messages.each_ref().map(ptr::from_ref);
    let mut output = ptr::null_mut();
    // SAFETY: Valid pointers allow exercising cleanup after one allocated password response.
    unsafe {
        assert_eq!(converse(2, pointers.as_mut_ptr(), &mut output, ptr::from_mut(&mut data).cast()), CONV_ERR);
        assert!(output.is_null());
        data.password.take();
        assert_eq!(converse(1, pointers.as_mut_ptr(), &mut output, ptr::from_mut(&mut data).cast()), CONV_ERR);
        assert!(output.is_null());
        assert_eq!(converse(0, ptr::null_mut(), &mut output, ptr::null_mut()), CONV_ERR);
    }
}

#[test]
fn outcomes_fail_closed_and_distinguish_account_and_delays() {
    assert_eq!(classify(0, true, ptr::null_mut()), Ok(Outcome::Success));
    for code in [6, 7, 10] {
        assert_eq!(classify(code, false, ptr::null_mut()), Ok(Outcome::WrongPassword));
        assert!(matches!(classify(code, true, ptr::null_mut()), Ok(Outcome::AccountProblem { .. })));
    }
    for code in [12, 13] {
        assert!(matches!(classify(code, true, ptr::null_mut()), Ok(Outcome::AccountProblem { .. })));
    }
    for code in [9, 11, 24, 30, 31] {
        assert!(matches!(classify(code, false, ptr::null_mut()), Ok(Outcome::TryAgainLater { .. })));
    }
    assert!(classify(26, false, ptr::null_mut()).is_err());
    assert!(classify(5, true, ptr::null_mut()).is_err());
}

#[test]
fn worker_rejects_busy_and_wakes_after_result_is_ready() {
    let (entered, entering) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (wake, woken) = mpsc::channel();
    let (auth, results) = Authenticator::start_with(
        move || {
            wake.send(()).unwrap();
        },
        move |_, _| {
            entered.send(()).unwrap();
            released.recv().unwrap();
            Ok(Outcome::Success)
        },
    )
    .unwrap();
    auth.submit("test-user".into(), secret()).unwrap();
    entering.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(auth.submit("test-user".into(), secret()), Err(SubmitError::Busy));
    release.send(()).unwrap();
    woken.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(results.try_recv().unwrap(), Ok(Outcome::Success));
    drop(auth);
}

#[test]
fn invalid_inputs_are_rejected_without_pam() {
    assert_eq!(run("a\0b", secret(), c"unused", None), Err(Error::InvalidUser));
    assert_eq!(run("", secret(), c"unused", None), Err(Error::InvalidUser));
    assert_eq!(run("test-user", SecretString::from("a\0b"), c"unused", None), Err(Error::InvalidPassword));
    assert!(!format!("{:?}", secret()).contains("synthetic-test-token"));
}

#[test]
#[allow(unsafe_code, reason = "the test checks whether Linux-PAM exports its optional private-config entry point")]
fn private_pam_services_permit_deny_and_account_deny() {
    // SAFETY: dlsym receives a valid static C string and its result is only checked for null.
    let available = unsafe { !libc::dlsym(libc::RTLD_DEFAULT, c"pam_start_confdir".as_ptr()).is_null() };
    if !available {
        eprintln!("Skipping private-confdir test: Linux-PAM lacks pam_start_confdir");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let confdir = CString::new(directory.path().as_os_str().as_encoded_bytes()).unwrap();
    for (name, auth_module, account_module) in [
        ("permit", "pam_permit.so", "pam_permit.so"),
        ("deny", "pam_deny.so", "pam_permit.so"),
        ("account-deny", "pam_permit.so", "pam_deny.so"),
    ] {
        std::fs::write(
            directory.path().join(name),
            format!("auth required {auth_module}\naccount required {account_module}\n"),
        )
        .unwrap();
        let service = CString::new(name).unwrap();
        let outcome = run("tileception-test-user", secret(), &service, Some(&confdir)).unwrap();
        match name {
            "permit" => assert_eq!(outcome, Outcome::Success),
            "deny" => assert_eq!(outcome, Outcome::WrongPassword),
            _ => assert!(matches!(outcome, Outcome::AccountProblem { .. })),
        }
    }
}

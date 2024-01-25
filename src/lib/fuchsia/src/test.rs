// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
mod test {

    use anyhow::Error;
    use argh::FromArgs;

    // TODO(anp): add or expand tests to ensure logging is initialized correctly.

    #[derive(FromArgs)]
    /// Test component.
    struct Options {
        #[argh(switch)]
        /// test argument that should always be off
        should_be_false: bool,
    }

    #[fuchsia::test]
    fn empty_test() {}

    #[fuchsia::test(logging = false)]
    fn empty_test_without_logging() {}

    #[fuchsia::test(logging = true)]
    fn empty_test_with_logging() {}

    #[fuchsia::test(logging_tags = ["foo", "bar"])]
    async fn empty_test_with_logging_tags() {}

    #[fuchsia::test(logging_tags = ["foo"])]
    async fn empty_test_with_single_logging_tag() {}

    #[fuchsia::test(logging_tags = [])]
    async fn empty_test_with_empty_logging_tags() {}

    #[fuchsia::test(logging_minimum_severity = "warn")]
    async fn empty_test_with_minimum_severity() {}

    #[allow(dead_code)]
    #[fuchsia::test(add_test_attr = false)]
    fn empty_test_without_add_test_attr() {
        // With `add_test_attr = false`, this function won't get a #[test] annotation, and therefore
        // is expected to _not_ run during tests.
        assert!(false)
    }

    #[test]
    #[fuchsia::test(add_test_attr = false)]
    fn empty_test_without_add_test_attr_with_explicit_test_attr_before() {}

    #[fuchsia::test(add_test_attr = false)]
    #[test]
    fn empty_test_without_add_test_attr_with_explicit_test_attr_after() {}

    #[fuchsia::test(add_test_attr = true)]
    fn empty_test_with_add_test_attr() {}

    #[fuchsia::test]
    async fn empty_async_test() {}

    #[fuchsia::test(threads = 1)]
    async fn empty_singlethreaded_test() {}

    #[fuchsia::test(threads = 2)]
    async fn empty_multithreaded_test() {}

    #[cfg(target_os = "fuchsia")]
    #[fuchsia::test(allow_stalls)]
    async fn empty_allow_stalls_test() {}

    #[cfg(target_os = "fuchsia")]
    #[fuchsia::test(allow_stalls = false)]
    async fn empty_not_allow_stalls_test() {}

    #[cfg(target_os = "fuchsia")]
    #[fuchsia::test(allow_stalls = true)]
    async fn empty_very_allow_stalls_test() {}

    #[cfg(target_os = "fuchsia")]
    #[fuchsia::test(allow_stalls = true, threads = 1)]
    async fn empty_very_singlethreaded_allow_stalls_test() {}

    #[fuchsia::main]
    #[test]
    fn empty_component_test() {}

    #[fuchsia::main]
    #[test]
    async fn empty_async_component_test() {}

    #[fuchsia::main(threads = 1)]
    #[test]
    async fn empty_async_singlethreaded_component_test() {}

    #[fuchsia::main(threads = 2)]
    #[test]
    async fn empty_async_multithreaded_component_test() {}

    #[fuchsia::test]
    fn empty_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    #[fuchsia::test]
    async fn empty_async_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    #[fuchsia::test(threads = 1)]
    async fn empty_singlethreaded_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    #[fuchsia::test(threads = 2)]
    async fn empty_multithreaded_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    #[cfg(target_os = "fuchsia")]
    #[fuchsia::test(allow_stalls = false)]
    async fn empty_not_allow_stalls_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    #[cfg(target_os = "fuchsia")]
    #[fuchsia::test(allow_stalls = true)]
    async fn empty_allow_stalls_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    #[cfg(target_os = "fuchsia")]
    #[fuchsia::test(allow_stalls = false, threads = 1)]
    async fn empty_very_singlethreaded_not_allow_stalls_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    #[allow(dead_code)]
    #[fuchsia::main(add_test_attr = true)]
    fn empty_component_test_with_add_test_attr() {
        // `add_test_attr = true` should only have an effect when using fuchsia::test, not
        // fuchsia::main. With fuchsia::main, this function shouldn't get a #[test]
        // annotation, and therefore is expected to _not_ run during tests.
        assert!(false)
    }

    // TODO(https://fxbug.dev/42161439): We combine #[fuchsia::main] and #[test] here as a kludge to
    // enable testing of the fuchsia::main code in a unit test.
    // Real users of the fuchsia library should not do this, and the ability to do so is not
    // guaranteed to be present in the future.
    #[fuchsia::main]
    #[test]
    fn empty_component_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    // TODO(https://fxbug.dev/42161439): We combine #[fuchsia::main] and #[test] here as a kludge to
    // enable testing of the fuchsia::main code in a unit test.
    // Real users of the fuchsia library should not do this, and the ability to do so is not
    // guaranteed to be present in the future.
    #[fuchsia::main]
    #[test]
    async fn empty_async_component_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    // TODO(https://fxbug.dev/42161439): We combine #[fuchsia::main] and #[test] here as a kludge to
    // enable testing of the fuchsia::main code in a unit test.
    // Real users of the fuchsia library should not do this, and the ability to do so is not
    // guaranteed to be present in the future.
    #[fuchsia::main(threads = 1)]
    #[test]
    async fn empty_async_singlethreaded_component_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    // TODO(https://fxbug.dev/42161439): We combine #[fuchsia::main] and #[test] here as a kludge to
    // enable testing of the fuchsia::main code in a unit test.
    // Real users of the fuchsia library should not do this, and the ability to do so is not
    // guaranteed to be present in the future.
    #[fuchsia::main(threads = 2)]
    #[test]
    async fn empty_async_multithreaded_component_test_with_result() -> Result<(), Error> {
        Ok(())
    }

    // TODO(https://fxbug.dev/42161439): We combine #[fuchsia::main] and #[test] here as a kludge to
    // enable testing of the fuchsia::main code in a unit test.
    // Real users of the fuchsia library should not do this, and the ability to do so is not
    // guaranteed to be present in the future.
    #[fuchsia::main(logging_tags = [])]
    #[test]
    async fn component_with_empty_logging_tags() -> Result<(), Error> {
        Ok(())
    }

    // TODO(https://fxbug.dev/42161439): We combine #[fuchsia::main] and #[test] here as a kludge to
    // enable testing of the fuchsia::main code in a unit test.
    // Real users of the fuchsia library should not do this, and the ability to do so is not
    // guaranteed to be present in the future.
    #[fuchsia::main(logging_tags = ["foo"])]
    #[test]
    async fn component_with_single_logging_tag() -> Result<(), Error> {
        Ok(())
    }

    // TODO(https://fxbug.dev/42161439): We combine #[fuchsia::main] and #[test] here as a kludge to
    // enable testing of the fuchsia::main code in a unit test.
    // Real users of the fuchsia library should not do this, and the ability to do so is not
    // guaranteed to be present in the future.
    #[fuchsia::main(logging_tags = ["foo", "bar"])]
    #[test]
    async fn component_with_logging_tags() -> Result<(), Error> {
        Ok(())
    }

    // TODO(https://fxbug.dev/42161439): We combine #[fuchsia::main] and #[test] here as a kludge to
    // enable testing of the fuchsia::main code in a unit test.
    // Real users of the fuchsia library should not do this, and the ability to do so is not
    // guaranteed to be present in the future.
    #[fuchsia::main(logging_minimum_severity = "error")]
    #[test]
    async fn component_with_minimum_severity() -> Result<(), Error> {
        Ok(())
    }

    // fuchsia::main with arguments can't be written as a test
    // (since argh will parse command line arguments and these will be arguments defining
    // the test execution environment)
    #[allow(dead_code)]
    #[fuchsia::main]
    fn empty_component_test_with_argument(opt: Options) {
        assert_eq!(opt.should_be_false, false);
    }

    // fuchsia::main with arguments can't be written as a test
    // (since argh will parse command line arguments and these will be arguments defining
    // the test execution environment)
    #[allow(dead_code)]
    #[fuchsia::main]
    async fn empty_async_component_test_with_argument(opt: Options) {
        assert_eq!(opt.should_be_false, false);
    }

    // fuchsia::main with arguments can't be written as a test
    // (since argh will parse command line arguments and these will be arguments defining
    // the test execution environment)
    #[allow(dead_code)]
    #[fuchsia::main(threads = 1)]
    async fn empty_async_singlethreaded_component_test_with_argument(opt: Options) {
        assert_eq!(opt.should_be_false, false);
    }

    // fuchsia::main with arguments can't be written as a test
    // (since argh will parse command line arguments and these will be arguments defining
    // the test execution environment)
    #[allow(dead_code)]
    #[fuchsia::main(threads = 2)]
    async fn empty_async_multithreaded_component_test_with_argument(opt: Options) {
        assert_eq!(opt.should_be_false, false);
    }

    // fuchsia::main with arguments can't be written as a test
    // (since argh will parse command line arguments and these will be arguments defining
    // the test execution environment)
    #[allow(dead_code)]
    #[fuchsia::main]
    fn empty_component_test_with_argument_and_result(opt: Options) -> Result<(), Error> {
        assert_eq!(opt.should_be_false, false);
        Ok(())
    }

    // fuchsia::main with arguments can't be written as a test
    // (since argh will parse command line arguments and these will be arguments defining
    // the test execution environment)
    #[allow(dead_code)]
    #[fuchsia::main]
    async fn empty_async_component_test_with_argument_and_result(
        opt: Options,
    ) -> Result<(), Error> {
        assert_eq!(opt.should_be_false, false);
        Ok(())
    }

    // fuchsia::main with arguments can't be written as a test
    // (since argh will parse command line arguments and these will be arguments defining
    // the test execution environment)
    #[allow(dead_code)]
    #[fuchsia::main(threads = 1)]
    async fn empty_async_singlethreaded_component_test_with_argument_and_result(
        opt: Options,
    ) -> Result<(), Error> {
        assert_eq!(opt.should_be_false, false);
        Ok(())
    }

    // fuchsia::main with arguments can't be written as a test
    // (since argh will parse command line arguments and these will be arguments defining
    // the test execution environment)
    #[allow(dead_code)]
    #[fuchsia::main(threads = 2)]
    async fn empty_async_multithreaded_component_test_with_argument_and_result(
        opt: Options,
    ) -> Result<(), Error> {
        assert_eq!(opt.should_be_false, false);
        Ok(())
    }
}

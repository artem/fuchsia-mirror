// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:math';
import 'package:flutter_driver/flutter_driver.dart';
import 'package:sl4f/sl4f.dart';
import 'package:test/test.dart';
import 'package:webdriver/sync_io.dart' show By;

import 'ermine_driver.dart';

const _timeoutSeconds = 10;
const _timeout = Duration(seconds: _timeoutSeconds);
const testserverUrl =
    'fuchsia-pkg://fuchsia.com/ermine_testserver#meta/ermine_testserver.cmx';

void main() {
  Sl4f sl4f;
  ErmineDriver ermine;

  final newTabFinder = find.text('NEW TAB');
  final indexTabFinder = find.text('Localhost');
  final nextTabFinder = find.text('Next Page');
  final popupTabFinder = find.text('Popup Page');
  final videoTabFinder = find.text('Video Test');
  final redTabFinder = find.text('Red Page');
  final greenTabFinder = find.text('Green Page');
  final blueTabFinder = find.text('Blue Page');

  setUpAll(() async {
    sl4f = Sl4f.fromEnvironment();
    await sl4f.startServer();

    ermine = ErmineDriver(sl4f);
    await ermine.setUp();

    // Starts hosting a local http website.
    // ignore: unawaited_futures
    ermine.component.launch(testserverUrl);
  });

  tearDownAll(() async {
    // Closes the test server.
    // simple-browser is launched via [Component.launch()] since it does not
    // have a view. Therefore, it cannot be closed with ermine's flutter driver.
    // For this reason, we have to explicitly stop the http server to avoid
    // HttpException which occurs in case the test is torn down still having it
    // running.
    // TODO(fxb/69291): Remove this workaround once we can properly close hidden
    // components
    FlutterDriver browser = await ermine.launchAndWaitForSimpleBrowser();
    await browser.requestData('http://127.0.0.1:8080/stop');

    await ermine.driver.requestData('closeAll');
    await ermine.driver.waitForAbsent(find.text('simple-browser.cmx'));

    final runningComponents = await ermine.component.list();
    expect(
        runningComponents.where((c) => c.contains(simpleBrowserUrl)).length, 0);
    expect(runningComponents.where((c) => c.contains(testserverUrl)).length, 0);

    await ermine.tearDown();

    await sl4f.stopServer();
    sl4f?.close();
  });

  Future<bool> _waitForColor(
    int hex, {
    Rectangle rect = const Rectangle(100, 100, 100, 100),
    Duration timeout = _timeout,
  }) async {
    final end = DateTime.now().add(timeout);
    while (DateTime.now().isBefore(end)) {
      final sampleImage = await ermine.screenshot(rect);
      bool isTheColor =
          sampleImage.data.every((pixel) => (pixel & hex) == (hex & hex));
      if (!isTheColor) {
        return true;
      }
    }
    return false;
  }

  // TODO(fxb/68689): Transition physical interactions to use Sl4f.Input once
  // fxb/69277 is fixed.
  test('Should be able to do page and history navigation.', () async {
    FlutterDriver browser;
    browser = await ermine.launchAndWaitForSimpleBrowser();

    // Access to the website.
    await browser.requestData('http://127.0.0.1:8080/index.html');

    await browser.waitFor(indexTabFinder, timeout: _timeout);
    final webdriver = await ermine.getWebDriverFor('127.0.0.1');

    expect(await browser.getText(newTabFinder), isNotNull);
    expect(await browser.getText(indexTabFinder), isNotNull);

    final nextLink = webdriver.findElement(By.linkText('Next'));
    expect(nextLink, isNotNull);

    // Clicks the text link that opens next.html (page navigation)
    nextLink.click();
    await browser.waitForAbsent(indexTabFinder, timeout: _timeout);
    expect(await browser.getText(newTabFinder), isNotNull);
    expect(await browser.getText(nextTabFinder), isNotNull);

    final prevLink = webdriver.findElement(By.linkText('Prev'));
    expect(prevLink, isNotNull);

    // Clicks the text link that opens index.html (page navigation)
    prevLink.click();
    await browser.waitForAbsent(nextTabFinder, timeout: _timeout);
    expect(await browser.getText(newTabFinder), isNotNull);
    expect(await browser.getText(indexTabFinder), isNotNull);

    // Goes back to next.html by tapping the BCK button (history navigation)
    final back = find.byValueKey('back');
    await browser.tap(back);
    await browser.waitForAbsent(indexTabFinder, timeout: _timeout);
    expect(await browser.getText(newTabFinder), isNotNull);
    expect(await browser.getText(nextTabFinder), isNotNull);

    // Goes forward to index.html by tapping the FWD button (history navigation)
    final forward = find.byValueKey('forward');
    await browser.tap(forward);
    await browser.waitForAbsent(nextTabFinder, timeout: _timeout);
    expect(await browser.getText(newTabFinder), isNotNull);
    expect(await browser.getText(indexTabFinder), isNotNull);

    // Clicks + button to increase the number
    var digitLink = webdriver.findElement(By.id('target'));
    final addButton = webdriver.findElement(By.id('increase'));
    expect(digitLink.text, '0');
    addButton.click();
    expect(digitLink.text, '1');
    addButton.click();
    expect(digitLink.text, '2');

    // Refreshes the page
    final refresh = find.byValueKey('refresh');
    await browser.tap(refresh);
    digitLink = webdriver.findElement(By.id('target'));
    expect(digitLink.text, '0');

    // Clicks the text link that opens popup.html (popup page navigation)
    final popupLink = webdriver.findElement(By.linkText('Popup'));
    expect(popupLink, isNotNull);

    popupLink.click();
    await browser.waitFor(popupTabFinder, timeout: _timeout);
    expect(await browser.getText(newTabFinder), isNotNull);
    expect(await browser.getText(indexTabFinder), isNotNull);
    expect(await browser.getText(popupTabFinder), isNotNull);

    await ermine.driver.requestData('close');
    await ermine.driver.waitForAbsent(find.text('simple-browser.cmx'));
    final runningComponents = await ermine.component.list();
    expect(
        runningComponents.where((c) => c.contains(simpleBrowserUrl)).length, 0);
  });

  test('Should be able to play videos on web pages.', () async {
    FlutterDriver browser;
    browser = await ermine.launchAndWaitForSimpleBrowser();

    // Access to video.html where the following video is played:
    // experiences/bin/ermine_testserver/public/simple_browser_test/sample_video.mp4
    // It shows the violet-colored background for the first 3 seconds then shows
    // the fuchsia-colored background for another 3 seconds.
    await browser.requestData('http://127.0.0.1:8080/video.html');

    await browser.waitFor(videoTabFinder, timeout: _timeout);

    expect(await browser.getText(videoTabFinder), isNotNull);

    // Takes the screenshot of the part of the video.
    // Only the color is changed in this area as the video is played.
    final viewRect = Rectangle(100, 100, 100, 100);

    // Waits for a while for the video to be loaded before taking a screenshot.
    await Future.delayed(Duration(seconds: 2));
    final earlyScreenshot = await ermine.screenshot(viewRect);

    // Takes another screenshot after 3 seconds.
    await Future.delayed(Duration(seconds: 3));
    final lateScreenshot = await ermine.screenshot(viewRect);

    final diff = ermine.screenshotsDiff(earlyScreenshot, lateScreenshot);
    expect(diff, 1, reason: 'The screenshots are more similar than expected.');

    await ermine.driver.requestData('close');
    await ermine.driver.waitForAbsent(find.text('simple-browser.cmx'));
    final runningComponents = await ermine.component.list();
    expect(
        runningComponents.where((c) => c.contains(simpleBrowserUrl)).length, 0);
  });

  test('Should be able to switch, rearrange, and close tabs', () async {
    FlutterDriver browser;
    browser = await ermine.launchAndWaitForSimpleBrowser();

    /// Tab Switching Test

    // TODO(fxb/69334): Get rid of the space in the hint text.
    const newTabHintText = '     SEARCH';
    const redUrl = 'http://127.0.0.1:8080/red.html';
    const greenUrl = 'http://127.0.0.1:8080/green.html';
    const blueUrl = 'http://127.0.0.1:8080/blue.html';

    // Opens red.html in the second tab leaving the first tab as an empty tab.
    await browser.requestData(redUrl);
    await browser.waitFor(redTabFinder, timeout: _timeout);

    // Opens green.html in the third tab.
    await browser.tap(find.byValueKey('new_tab'));
    await browser.waitFor(find.text(newTabHintText), timeout: _timeout);
    await browser.requestData(greenUrl);
    await browser.waitFor(greenTabFinder, timeout: _timeout);

    // Opens blue.html in the forth tab.
    await browser.tap(find.byValueKey('new_tab'));
    await browser.waitFor(find.text(newTabHintText), timeout: _timeout);
    await browser.requestData(blueUrl);
    await browser.waitFor(blueTabFinder, timeout: _timeout);

    // Should have 4 tabs and the forth tab should be focused.
    expect(await browser.getText(newTabFinder), isNotNull);
    expect(await browser.getText(redTabFinder), isNotNull);
    expect(await browser.getText(greenTabFinder), isNotNull);
    expect(await browser.getText(blueTabFinder), isNotNull);
    expect(await browser.getText(find.text(blueUrl)), isNotNull);

    // The second tab should be focused when tapped.
    await browser.tap(redTabFinder);
    await browser.waitFor(find.text(redUrl));
    expect(await browser.getText(find.text(redUrl)), isNotNull);

    // The thrid tab should be focused when tapped.
    await browser.tap(greenTabFinder);
    await browser.waitFor(find.text(greenUrl));
    expect(await browser.getText(find.text(greenUrl)), isNotNull);

    // Makes sure the web page is loaded before proceeding image diff tests.
    final isGreen = await _waitForColor(0x0000ff00);
    expect(isGreen, true, reason: 'The green web page has not been loaded.');

    /// Tab Rearranging Test

    const String goldenBefore = 'simple_browser_rearranging_tab_before.png';
    const String goldenAfter = 'simple_browser_rearranging_tab_after.png';

    // Gets the view rect.
    final viewRect = await ermine.getViewRect(simpleBrowserUrl);

    // Takes a screenshot before rearranging a tab.
    final screenshotBefore = await ermine.screenshot(viewRect);

    // TODO(fxb/70233): This is a temporary line to see if fxb/70233 is also
    // observed on the FYI builders. Remove it once it's confirmed.
    ermine.saveImageAs(screenshotBefore, goldenBefore);
    final goldenDiffBefore = ermine.goldenDiff(screenshotBefore, goldenBefore);

    // TODO(fxb/70233): The value should be 0 once 1920x1080 golden is ready.
    expect(goldenDiffBefore < 0.01, true,
        reason: 'Failed at the scuba test with $goldenBefore at the diff rate'
            ' of $goldenDiffBefore');

    // Drags the second tab to the right end of the tab list.
    await browser.scroll(redTabFinder, 600, 0, Duration(seconds: 1));

    final isRed = await _waitForColor(0x00ff0000);
    expect(isRed, true, reason: 'The red web page has not been loaded.');

    // Takes a screenshot after rearranging the tab.
    final screenshotAfter = await ermine.screenshot(viewRect);
    final goldenDiffAfter = ermine.goldenDiff(screenshotAfter, goldenAfter);
    // TODO(fxb/70233): The value should be 0 once 1920x1080 golden is ready.
    expect(goldenDiffAfter < 0.01, true,
        reason: 'Failed at the scuba test with $goldenAfter at the diff rate of'
            ' $goldenDiffAfter');

    // TODO(fxb/68719): Test tab closing.

    await ermine.driver.requestData('close');
    await ermine.driver.waitForAbsent(find.text('simple-browser.cmx'));
    final runningComponents = await ermine.component.list();
    expect(
        runningComponents.where((c) => c.contains(simpleBrowserUrl)).length, 0);
  });

  // TODO(fxb/68720): Test web editing
  // TODO(fxb/68716): Test audio playing
}

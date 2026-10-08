// Run against remote-workspace-browser.html. All settings and failures use the mock API.
async page => {
  const assert = (ok, message) => { if (!ok) throw new Error(message); };
  const ready = async () => {
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.waitForFunction(() => { const select = document.querySelector('select[aria-label="模型"]'); return select && !select.disabled; });
  };
  const gauge = page.getByRole('button', { name: '调整模型与思考强度' });
  const slider = page.getByRole('slider', { name: '思考强度' });
  const advanced = async () => {
    await gauge.click(); await page.getByRole('button', { name: /^高级设置：/ }).click();
    await page.getByRole('dialog', { name: '高级设置' }).waitFor();
  };
  const close = () => page.getByRole('button', { name: '关闭高级设置' }).click();
  const layout = async selector => {
    const bounds = await page.locator(selector).boundingBox();
    const viewport = page.viewportSize();
    assert(bounds && bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= viewport.width + 1 && bounds.y + bounds.height <= viewport.height + 1, `${selector} escaped the viewport`);
  };
  const refreshCatalog = async () => {
    await page.getByRole('button', { name: '返回会话列表' }).click();
    await page.getByRole('button', { name: '刷新会话' }).click();
    await page.getByRole('button', { name: /重新设计界面/ }).click(); await ready();
  };
  await page.evaluate(() => sessionStorage.clear()); await page.reload();
  await page.setViewportSize({ width: 390, height: 844 }); await page.emulateMedia({ colorScheme: 'light' });
  await page.getByRole('button', { name: /重新设计界面/ }).waitFor();
  await page.evaluate(() => { window.remoteFixture.catalog.model_metadata[0].model_display_name = 'route-mu7vzcl4-l2l5kj/gpt-6-astra'; });
  await page.getByRole('button', { name: '刷新会话' }).click();
  await page.getByRole('button', { name: /重新设计界面/ }).click(); await ready();
  await page.getByLabel('权限', { exact: true }).selectOption('full-access'); await ready();
  await advanced();
  await page.getByLabel('思考程度', { exact: true }).selectOption('max'); await ready();
  await page.getByLabel('速度模式', { exact: true }).selectOption('priority'); await ready();
  await layout('.remote-advanced-sheet');
  await page.screenshot({ path: 'output/playwright/remote-composer-advanced-light.png' });
  await page.locator('.remote-advanced-sheet').screenshot({ path: 'output/playwright/remote-composer-advanced-detail.png' });
  for (let i = 0; i < 6; i++) await page.keyboard.press('Tab');
  assert(await page.getByRole('dialog').evaluate(el => el.contains(document.activeElement)), 'Focus escaped the modal');
  await page.mouse.click(4, 4);
  assert(await page.getByRole('dialog').count() === 0, 'Backdrop did not dismiss the sheet');
  await page.screenshot({ path: 'output/playwright/remote-composer-light.png' });
  await page.locator('.remote-composer').screenshot({ path: 'output/playwright/remote-composer-detail.png' });
  await gauge.click(); await layout('.remote-power-popover');
  await page.screenshot({ path: 'output/playwright/remote-composer-power-light.png' });
  await page.locator('.remote-power-popover').screenshot({ path: 'output/playwright/remote-composer-power-detail.png' });
  await page.getByRole('heading', { name: '重新设计界面' }).click();
  assert(await slider.count() === 0, 'Outside click did not dismiss the popover');
  await page.getByRole('button', { name: '添加附件', exact: true }).click(); await layout('.remote-attachment-popover');
  await page.screenshot({ path: 'output/playwright/remote-composer-attachments-light.png' });
  await page.locator('.remote-attachment-popover').screenshot({ path: 'output/playwright/remote-composer-attachments-detail.png' });
  await page.keyboard.press('Escape');
  await page.emulateMedia({ colorScheme: 'dark' });
  await gauge.click(); await page.screenshot({ path: 'output/playwright/remote-composer-power-dark.png' });
  await page.getByRole('button', { name: /^高级设置：/ }).click();
  await page.screenshot({ path: 'output/playwright/remote-composer-advanced-dark.png' }); await close();
  await page.emulateMedia({ colorScheme: 'light' });
  for (const viewport of [{ width: 320, height: 568 }, { width: 390, height: 450 }, { width: 1440, height: 900 }]) {
    await page.setViewportSize(viewport); await layout('.remote-composer');
    await gauge.click(); await layout('.remote-power-popover');
    await page.getByRole('button', { name: /^高级设置：/ }).click(); await layout('.remote-advanced-sheet');
    await page.screenshot({ path: `output/playwright/remote-composer-sheet-${viewport.width}x${viewport.height}.png` }); await close();
  }
  await page.setViewportSize({ width: 390, height: 450 });
  await page.getByLabel('发送给 Codex 的指令').fill('多行草稿\n'.repeat(14));
  await gauge.click(); await layout('.remote-power-popover'); await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '添加附件', exact: true }).click(); await layout('.remote-attachment-popover');
  await page.getByRole('button', { name: '照片', exact: true }).click();
  await page.getByText('照片功能暂未开放', { exact: true }).waitFor();
  await page.getByLabel('发送给 Codex 的指令').fill('');
  await page.setViewportSize({ width: 390, height: 844 });
  await gauge.click();
  const beforeFailure = await page.getByLabel('思考程度', { exact: true }).inputValue();
  await page.evaluate(() => { window.remoteFixture.failNext = true; });
  await slider.focus(); await page.keyboard.press('Home'); await ready();
  await page.getByRole('alert').filter({ hasText: '模拟连接中断' }).waitFor();
  assert(await page.getByLabel('思考程度', { exact: true }).inputValue() === beforeFailure, 'Failed settings overwrote the confirmed effort');
  assert(await slider.getAttribute('aria-valuetext') === 'Max', 'Failed settings left an optimistic slider value');
  await page.evaluate(() => window.remoteFixture.unavailable());
  await page.getByText('桌面暂时无法读取此会话', { exact: true }).waitFor();
  assert(await slider.getAttribute('aria-disabled') === 'true', 'Disconnected slider remained editable');
  const disconnectedActions = await page.evaluate(() => window.remoteFixture.actions.length);
  await slider.focus(); await page.keyboard.press('End');
  assert(await page.evaluate(() => window.remoteFixture.actions.length) === disconnectedActions, 'Disconnected slider submitted settings');
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '重新连接' }).click(); await ready();
  for (const efforts of [['max'], []]) {
    await page.evaluate(efforts => { window.remoteFixture.catalog.model_metadata[0].supported_reasoning_efforts = efforts; }, efforts);
    await refreshCatalog(); await gauge.click();
    assert(await slider.isDisabled(), 'A model with fewer than two efforts has an active slider');
    if (!efforts.length) await page.getByText('当前模型未提供可调整的思考强度').waitFor();
    await page.keyboard.press('Escape');
  }
  await page.evaluate(() => { window.remoteFixture.catalog.model_metadata[0].supported_reasoning_efforts = ['low', 'medium', 'high', 'xhigh', 'max', 'ultra']; });
  await refreshCatalog();
  await page.evaluate(() => { window.remoteFixture.catalog.model_metadata[0].supported_reasoning_efforts = ['ultra', 'low', 'max']; });
  await refreshCatalog(); await gauge.click();
  await slider.focus(); await page.keyboard.press('Home'); await ready();
  assert(await slider.getAttribute('aria-valuetext') === '低', 'Unordered catalog put a high effort at the left of the slider');
  await page.keyboard.press('End'); await ready();
  assert(await slider.getAttribute('aria-valuetext') === 'Ultra', 'Unordered catalog put a low effort at the right of the slider');
  await page.keyboard.press('Escape');
  await page.evaluate(() => { window.remoteFixture.catalog.model_metadata[0].supported_reasoning_efforts = ['low', 'medium', 'high', 'xhigh', 'max', 'ultra']; });
  await refreshCatalog();
  return { passed: ['Four reference states', 'Long model names', 'Light and dark', '320px, keyboard and desktop layouts', 'Modal focus trap and dismissal', 'Failed slider update rollback', 'Disconnected slider guard', 'Single, absent and unordered reasoning levels'] };
}

async ({ page, record }) => {
  await page.waitForTimeout(1000);
  record.stop();
  return 'stopped';
}

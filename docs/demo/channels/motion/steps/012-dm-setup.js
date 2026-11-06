async ({page,context,mark,out,record,profile}) => {
await page.getByRole('button',{name:'Close thread',exact:true}).click();await page.getByRole('navigation',{name:'Direct messages',exact:true}).getByRole('button').filter({hasText:'@claude'}).click();const box=page.getByRole('combobox',{name:'Message @claude privately',exact:true});
for(const text of ['We are preparing Channels Beta. What should we check first? Reply in one short sentence.','Keep this conversation for release reminders. Acknowledge briefly.','What should be ready before release? Keep the answer to one short sentence.','For the final check, focus on tests, release notes and the rollback plan. Acknowledge briefly.']){const count=await page.locator('article[data-message-id]').count();await box.fill(text);await box.press('Enter');await page.waitForFunction(n=>document.querySelectorAll('article[data-message-id]').length>=n,count+2,{timeout:90000});}
await mark('dm-open');return await page.locator('body').innerText();

}

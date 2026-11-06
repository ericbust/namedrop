async ({page,context,mark,out,record,profile}) => {
const members=page.getByRole('list',{name:'Members',exact:true});
const text=await members.innerText();if(text.includes('Offline')||text.includes('Working')||text.includes('Needs you'))throw new Error('unready agents');
await record.start();await mark('wide');await page.waitForTimeout(2800);
const box=page.getByRole('combobox',{name:'Message #release-planning',exact:true});
const b=await box.boundingBox(),last=await page.locator('article[data-message-id]').last().boundingBox();
const rect={x:b.x,y:last?Math.max(120,last.y-15):b.y-150,width:b.width,height:b.y+b.height-(last?Math.max(120,last.y-15):b.y-150)};
await mark('ask-type',rect);await box.pressSequentially('@codex Give a final three-check release checklist in one short sentence.',{delay:90});await box.press('Escape');await mark('ask-instruction',rect);await page.waitForTimeout(1800);
const previews=await page.locator('.reply-preview').count();await box.press('Enter');await mark('ask-posted');
await page.waitForFunction(()=>[...document.querySelectorAll('.members .member')].some(n=>n.textContent.includes('@codex')&&n.textContent.includes('Working')),{},{timeout:60000});await mark('ask-working');
await page.waitForFunction(n=>document.querySelectorAll('.reply-preview').length>n,previews,{timeout:120000});await mark('ask-reply',await page.locator('.reply-preview').last().boundingBox());await page.waitForTimeout(2200);await page.locator('.reply-preview').last().click();await mark('thread-open',await page.getByRole('complementary',{name:'Thread',exact:true}).boundingBox());return await page.locator('body').innerText();

}

async ({page,context,mark,out,record,profile}) => {
await page.getByRole('navigation',{name:'Channel list',exact:true}).getByRole('button').filter({hasText:'Release planning'}).first().click();
await page.waitForTimeout(2000);const box=page.getByRole('combobox',{name:'Message #release-planning',exact:true});
await box.pressSequentially('@claude Use Bash to run touch release-notes-final.txt in this repository.',{delay:85});await box.press('Escape');await box.press('Enter');await mark('needs-request');
const needs=page.getByRole('navigation',{name:'Needs you',exact:true});await needs.waitFor({timeout:90000});await mark('needs-blocked',await needs.boundingBox());
const dm=page.getByRole('navigation',{name:'Direct messages',exact:true}).getByRole('button').filter({hasText:'@claude'});const member=page.getByRole('list',{name:'Members',exact:true}).getByRole('button').filter({hasText:'@claude'});const a=await dm.innerText(),b=await member.innerText();if(!a.includes('Needs you')||!b.includes('Needs you'))throw new Error('status disagreement '+a+' '+b);
await needs.getByRole('button').first().click();const banner=page.getByText('@claude is waiting on an approval or question.',{exact:true}).last();await banner.waitFor();await mark('needs-open',await banner.boundingBox());await page.waitForTimeout(3000);await page.screenshot({path:out+'/needs-you.png'});const text=await page.locator('body').innerText();await record.cancel('claude');return text;

}

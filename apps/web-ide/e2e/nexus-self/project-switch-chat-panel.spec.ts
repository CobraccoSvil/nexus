/**
 * Regressione: il pannello AI Workspace segue il cambio progetto senza reload.
 *
 * Incidente 2026-07-20: selezionando un altro progetto dal combobox, header e
 * terminale passavano al progetto nuovo ma il pannello chat restava sulla chat
 * (e sul selettore chat) del progetto precedente; un messaggio digitato li'
 * non produceva alcun POST. Causa: useMultiChat non resettava lo stato al
 * cambio di projectId e un bootstrap fallito congelava il pannello per sempre.
 *
 * Il test richiede almeno 2 progetti registrati nell'istanza: altrimenti skip.
 */
import { test, expect } from "@playwright/test";
import { setAuthCookie } from "./_setup";

test.beforeEach(async ({ context, baseURL }) => {
  await setAuthCookie(context, baseURL!);
});

const optionValues = "option:not([value=''])";

test("il selettore chat si riaggancia al progetto selezionato", async ({ page }) => {
  await page.setViewportSize({ width: 1600, height: 900 });
  await page.goto("/ide");

  const projectSelect = page.getByLabel("Selettore progetto");
  await expect(projectSelect).toBeVisible({ timeout: 15_000 });

  const projectIds = await projectSelect
    .locator(optionValues)
    .evaluateAll((opts) => opts.map((o) => (o as HTMLOptionElement).value));
  test.skip(projectIds.length < 2, "servono almeno 2 progetti registrati");

  // La testata chat puo' essere inline (select sempre nel DOM) o collassata
  // nel popover (select montato solo ad hamburger aperto): stessa aria-label.
  const chatSelect = page.getByLabel("Seleziona sessione chat");
  const openChatHeadIfCollapsed = async () => {
    if ((await chatSelect.count()) === 0) {
      await page.getByTitle("Testata chat: profilo, sessioni e azioni").click();
    }
  };

  await openChatHeadIfCollapsed();
  await expect(chatSelect).toBeVisible({ timeout: 15_000 });
  // Bootstrap del progetto iniziale completo: almeno una sessione in lista
  // (il bootstrap auto-crea "Chat 1" se il progetto non ne ha).
  await expect(chatSelect.locator(optionValues).first()).toBeAttached({ timeout: 20_000 });
  const sessionsBefore = await chatSelect
    .locator(optionValues)
    .evaluateAll((opts) => opts.map((o) => (o as HTMLOptionElement).value));

  const currentProject = await projectSelect.inputValue();
  const targetProject = projectIds.find((id) => id !== currentProject);
  expect(targetProject).toBeTruthy();
  await projectSelect.selectOption(targetProject!);

  // Il selettore chat deve ripopolarsi con sessioni del progetto nuovo:
  // nessuna intersezione con gli id del progetto precedente e almeno una
  // sessione presente (auto-create inclusa). Prima del fix restava congelato
  // sulle sessioni del progetto precedente.
  await expect
    .poll(
      async () => {
        await openChatHeadIfCollapsed();
        const after = await chatSelect
          .locator(optionValues)
          .evaluateAll((opts) => opts.map((o) => (o as HTMLOptionElement).value))
          .catch(() => [] as string[]);
        if (after.length === 0) return "vuoto";
        return after.some((id) => sessionsBefore.includes(id)) ? "stantio" : "ok";
      },
      { timeout: 30_000 },
    )
    .toBe("ok");
});

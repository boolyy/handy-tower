// handy-tower: the fork's strings live outside the upstream locale files, so a
// rebase never conflicts there. They are merged into English at startup; every
// other language falls back to English for them (fallbackLng: "en").
import i18n from "./index";
import towerEn from "./tower.en.json";

i18n.addResourceBundle("en", "translation", towerEn, true, false);

export default i18n;

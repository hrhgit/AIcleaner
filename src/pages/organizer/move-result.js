import { getLang } from '../../utils/i18n.js';
import { MOVE_RESULT_TEXT } from './constants.js';

export function getMoveResultText(key) {
  const lang = getLang();
  return MOVE_RESULT_TEXT[lang]?.[key] || MOVE_RESULT_TEXT.en[key] || key;
}

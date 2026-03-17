import { formatName } from './utils';
import type { User } from './types';

export function greet(user: User): string {
    return `Hello, ${formatName(user)}!`;
}

export const VERSION = '1.0.0';

import type { User } from './types';

export function formatName(user: User): string {
    return `${user.firstName} ${user.lastName}`;
}

export function capitalize(s: string): string {
    return s.charAt(0).toUpperCase() + s.slice(1);
}

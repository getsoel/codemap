export interface User {
    firstName: string;
    lastName: string;
    email: string;
}

export type UserId = string;

export enum Role {
    Admin,
    User,
    Guest,
}

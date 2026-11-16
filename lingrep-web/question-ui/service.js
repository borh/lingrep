export const serviceURL = (path, base = document.baseURI) => new URL(path.replace(/^\/+/, ''), base).href;
export class ServiceError extends Error {
    status;
    constructor(status, message) {
        super(message);
        this.status = status;
    }
}
export const expiredSearch = (error) => error instanceof ServiceError && (error.status === 404 || error.status === 410);

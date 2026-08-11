import express, { Request, Response, NextFunction } from 'express';
import { userController } from './controllers/userController';

const app = express();

// Typed route with path params
app.get('/api/users/:id', userController.getById);
app.post('/api/users', userController.create);
app.put('/api/users/:id', userController.update);

// Router with explicit type
const router: express.Router = express.Router();
router.get('/items', itemsController.index);
router.post('/items', itemsController.store);

// Chained route builder with types
app.route('/api/photos')
    .get(listPhotos)
    .post(createPhoto);

// Mounted sub-router
app.use('/api/v1', v1Router);

// Inline typed handler (should be ignored)
app.get('/status', (req: Request, res: Response) => {
    res.json({ status: 'ok' });
});

export { app, router };

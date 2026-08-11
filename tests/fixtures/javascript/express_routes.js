const express = require('express');
const app = express();

// Direct method calls
app.get('/users', usersController.list);
app.post('/users', usersController.create);
app.put('/users/:id', usersController.update);
app.delete('/users/:id', usersController.delete);
app.patch('/users/:id', usersController.partialUpdate);

// app.use — middleware
app.use('/api', apiRouter);
app.use(authMiddleware);

// app.all — catch-all
app.all('/health', healthHandler);

// Chained route builder
app.route('/photos')
    .get(listPhotos)
    .post(createPhoto)
    .delete(deletePhoto);

// Router instance
const router = express.Router();
router.get('/items', itemsController.index);
router.post('/items', itemsController.store);

// Arrow function handler (should be ignored since inline)
app.get('/inline', (req, res) => { res.send('ok'); });

// Async handler
app.get('/async', asyncHandler(fetchData));

module.exports = { app, router };
